// thought 前端主逻辑：登录、会话（SSE 流式）、记忆、系统视图。

import { api, loadAuth, saveAuth, clearAuth, setUnauthorizedHandler } from './api.js';
import { connectSSE } from './sse.js';
import { renderMarkdown } from './markdown.js';

const $ = (sel) => document.querySelector(sel);

const state = {
  auth: null,            // {token, expires_at, user}
  sessions: [],
  currentId: null,
  view: 'chat',
  sse: null,             // 当前 SSE 连接
  sseRetry: 0,
  sseTimer: null,
  run: null,             // 当前运行块上下文
  toolCards: new Map(),  // call_id → 工具卡片（跨 run，按 id 全局配对）
  runningTasks: new Map(), // task_id → 任务卡片上下文
  memoryLoaded: false,
  memoryEnabled: true,
  memoryTab: 'search',
  systemLoaded: false,
};

// ==================== 工具函数 ====================

function el(tag, className, text) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function relTime(ms) {
  const diff = Date.now() - ms;
  if (diff < 60e3) return '刚刚';
  if (diff < 3600e3) return `${Math.floor(diff / 60e3)} 分钟前`;
  if (diff < 86400e3) return `${Math.floor(diff / 3600e3)} 小时前`;
  const d = new Date(ms);
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}

function fmtElapsed(startMs, endMs = Date.now()) {
  const s = Math.max(0, Math.round((endMs - startMs) / 1000));
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m${s % 60}s`;
}

// 消息区接近底部时才跟随滚动，避免打断用户翻历史
function scrollBottom(force = false) {
  const box = $('#messages');
  const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 160;
  if (force || nearBottom) box.scrollTop = box.scrollHeight;
}

// ==================== 登录 ====================

function showLogin() {
  closeSSE();
  state.auth = null;
  state.currentId = null;
  state.sessions = [];
  $('#app').classList.add('hidden');
  $('#login-view').classList.remove('hidden');
  $('#login-password').value = '';
  $('#login-error').classList.add('hidden');
}

function showApp() {
  $('#login-view').classList.add('hidden');
  $('#app').classList.remove('hidden');
  const user = state.auth.user;
  $('#user-name').textContent = user.username;
  $('#user-avatar').textContent = (user.username[0] || '?').toUpperCase();
  const role = $('#user-role');
  role.textContent = user.role;
  role.classList.toggle('admin', user.role === 'admin');
  loadSessions();
}

async function doLogin(e) {
  e.preventDefault();
  const btn = $('#login-submit');
  const err = $('#login-error');
  err.classList.add('hidden');
  btn.disabled = true;
  btn.textContent = '登录中…';
  try {
    const res = await api('/api/auth/login', {
      method: 'POST',
      skip401: true,
      body: { username: $('#login-username').value.trim(), password: $('#login-password').value },
    });
    state.auth = { token: res.token, expires_at: res.expires_at, user: res.user };
    saveAuth(state.auth);
    showApp();
  } catch (e2) {
    err.textContent = e2.status === 401 ? '用户名或密码错误' : `登录失败：${e2.message}`;
    err.classList.remove('hidden');
  } finally {
    btn.disabled = false;
    btn.textContent = '登 录';
  }
}

function logout() {
  clearAuth();
  showLogin();
}

// ==================== 会话列表 ====================

async function loadSessions() {
  try {
    state.sessions = await api('/api/sessions');
    renderSessionList();
  } catch { /* 401 已全局处理；其余错误下次操作时再报 */ }
}

function renderSessionList() {
  const list = $('#session-list');
  list.textContent = '';
  for (const s of state.sessions) {
    const item = el('div', 'session-item' + (s.id === state.currentId ? ' active' : ''));
    item.append(el('span', 'session-title', s.title || '新会话'));
    item.append(el('span', 'session-time', relTime(s.updated_at_ms)));
    const del = el('button', 'session-del', '×');
    del.title = '删除会话';
    del.addEventListener('click', (e) => {
      e.stopPropagation();
      deleteSession(s);
    });
    item.append(del);
    item.addEventListener('click', () => selectSession(s.id));
    list.append(item);
  }
}

async function createSession() {
  const s = await api('/api/sessions', { method: 'POST' });
  await loadSessions();
  await selectSession(s.id);
  switchView('chat');
  $('#input').focus();
}

async function deleteSession(s) {
  if (!confirm(`删除会话「${s.title || '新会话'}」？此操作不可恢复。`)) return;
  await api(`/api/sessions/${encodeURIComponent(s.id)}`, { method: 'DELETE' });
  if (state.currentId === s.id) {
    state.currentId = null;
    closeSSE();
    clearMessages('选择左侧会话，或新建一个开始对话');
    $('#chat-title').textContent = '会话';
    setConn('off', '');
  }
  await loadSessions();
}

// ==================== 会话视图 ====================

async function selectSession(id) {
  if (state.currentId === id && state.sse) return;
  closeSSE();
  state.sseRetry = 0;
  state.currentId = id;
  state.run = null;
  state.toolCards.clear();
  state.runningTasks.clear();
  updateTasksBadge();
  renderSessionList();

  const s = state.sessions.find((x) => x.id === id);
  $('#chat-title').textContent = s ? s.title || '新会话' : '会话';
  clearMessages('加载中…');

  try {
    const detail = await api(`/api/sessions/${encodeURIComponent(id)}`);
    clearMessages();
    renderHistory(detail.history || []);
    // 恢复仍在运行的任务卡片（事件无回放，只能拿到清单）
    const tasks = await api(`/api/sessions/${encodeURIComponent(id)}/tasks`);
    if (tasks.length > 0) {
      const run = ensureRun();
      for (const t of tasks) ensureTaskCard(run, String(t.task_id), t.worker, t.task);
    }
    if (!$('#messages').children.length) {
      clearMessages('这个会话还没有消息，从下方输入开始');
    }
    scrollBottom(true);
    connectEvents();
  } catch (e) {
    clearMessages(`加载会话失败：${e.message}`);
  }
}

function clearMessages(placeholder) {
  const box = $('#messages');
  box.textContent = '';
  if (placeholder) box.append(el('div', 'placeholder', placeholder));
}

function removePlaceholder() {
  $('#messages .placeholder')?.remove();
}

function addUserBlock(content) {
  removePlaceholder();
  const msg = el('div', 'msg msg-user');
  msg.append(el('span', 'msg-label', '你'));
  msg.append(el('span', null, content));
  $('#messages').append(msg);
  scrollBottom();
}

// ---- 运行块 ----

function ensureRun() {
  if (state.run && !state.run.done) return state.run;
  removePlaceholder();
  const runEl = el('div', 'msg run-block streaming');

  const ctrlLabel = el('div', 'stream-label', '主控');
  const reasonEl = el('div', 'reasoning-stream hidden');
  const textEl = el('div', 'text-stream streaming-cursor');
  const toolsEl = el('div', 'run-tools');
  const tasksEl = el('div', 'run-tasks');
  const usageEl = el('div', 'usage-line hidden');
  runEl.append(ctrlLabel, reasonEl, textEl, toolsEl, tasksEl, usageEl);
  $('#messages').append(runEl);
  scrollBottom();

  state.run = {
    el: runEl, textEl, reasonEl, toolsEl, tasksEl, usageEl,
    usage: { prompt: 0, completion: 0 },
    tasks: new Map(),        // task_id → 任务卡片上下文
    done: false,
    replied: false,
  };
  return state.run;
}

// agent 标签：'controller' | 'worker#taskId' | 'worker#taskId/subN'
function parseAgent(label) {
  if (!label || label === 'controller') return null;
  const head = label.split('/')[0];
  const [worker, taskId] = head.split('#');
  return { worker, taskId: taskId || '' };
}

// 任务卡片：task_started 或首个属于该任务的事件触发创建
function ensureTaskCard(run, taskId, worker, taskText) {
  if (run.tasks.has(taskId)) return run.tasks.get(taskId);

  const card = el('div', 'task-card running');
  card.dataset.start = String(Date.now());
  const head = el('div', 'task-head');
  const status = el('span', 'task-status');
  head.append(status, el('span', 'task-worker', `${worker}#${taskId}`));
  head.append(el('span', 'task-text', taskText || ''));
  const elapsed = el('span', 'task-elapsed', '0s');
  head.append(elapsed, el('span', 'task-toggle', '▼'));
  const body = el('div', 'task-body');
  card.append(head, body);
  head.addEventListener('click', () => card.classList.toggle('closed'));
  run.tasksEl.append(card);
  scrollBottom();

  const ctx = { el: card, bodyEl: body, statusEl: status, elapsedEl: elapsed, agentStreams: new Map() };
  run.tasks.set(taskId, ctx);
  state.runningTasks.set(taskId, ctx);
  updateTasksBadge();
  return ctx;
}

// 任务卡片内某个 agent（含子 agent）的流式区域
function agentStreamEl(taskCtx, label, isReasoning) {
  const key = `${label}${isReasoning ? '|r' : ''}`;
  if (taskCtx.agentStreams.has(key)) return taskCtx.agentStreams.get(key);
  const wrap = el('div', 'agent-sub');
  wrap.append(el('div', 'stream-label', label));
  const stream = el('div', isReasoning ? 'reasoning-stream streaming-cursor' : 'text-stream streaming-cursor');
  wrap.append(stream);
  taskCtx.bodyEl.append(wrap);
  taskCtx.agentStreams.set(key, stream);
  return stream;
}

function onDelta(ev, isReasoning) {
  const run = ensureRun();
  const loc = parseAgent(ev.agent);
  let target;
  if (!loc) {
    target = isReasoning ? run.reasonEl : run.textEl;
    target.classList.remove('hidden');
  } else {
    target = agentStreamEl(ensureTaskCard(run, loc.taskId, loc.worker), ev.agent, isReasoning);
  }
  target.appendChild(document.createTextNode(ev.text));
  scrollBottom();
}

// ---- 工具调用卡片 ----

function toolContainerFor(agent) {
  const run = ensureRun();
  const loc = parseAgent(agent);
  return loc ? ensureTaskCard(run, loc.taskId, loc.worker).bodyEl : run.toolsEl;
}

function prettyArgs(raw) {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

function addToolCard(container, { id, name, args, output, readonly }) {
  const card = el('div', 'tool-card closed' + (output === undefined ? ' pending' : ' done'));
  const head = el('div', 'tool-head');
  head.append(el('span', 'tool-status', output === undefined ? '⏳' : '✓'));
  head.append(el('span', 'tool-name', name || ''));
  const summary = el('span', 'tool-summary', (args || '').slice(0, 120));
  head.append(summary, el('span', 'tool-toggle', '▼'));
  head.addEventListener('click', () => card.classList.toggle('closed'));

  const detail = el('div', 'tool-detail');
  if (args) {
    detail.append(el('div', 'tool-section-label', '参数'));
    detail.append(el('pre', null, prettyArgs(args)));
  }
  const outWrap = el('div', 'tool-output-wrap collapsed hidden');
  const outPre = el('pre');
  const expandBtn = el('button', 'tool-expand', '展开全部');
  expandBtn.type = 'button';
  expandBtn.addEventListener('click', () => {
    const collapsed = outWrap.classList.toggle('collapsed');
    expandBtn.textContent = collapsed ? '展开全部' : '收起';
  });
  outWrap.append(outPre, expandBtn);
  detail.append(outWrap);
  card.append(head, detail);
  container.append(card);
  scrollBottom();

  const ctx = { el: card, outWrap, outPre, readonly };
  state.toolCards.set(id, ctx);
  if (output !== undefined) fillToolOutput(ctx, output);
  return ctx;
}

function fillToolOutput(ctx, output) {
  ctx.el.classList.remove('pending');
  ctx.el.classList.add('done');
  ctx.el.querySelector('.tool-status').textContent = '✓';
  ctx.outPre.textContent = output;
  ctx.outWrap.classList.remove('hidden');
  scrollBottom();
}

function onToolCall(ev) {
  addToolCard(toolContainerFor(ev.agent), { id: ev.id, name: ev.name, args: ev.arguments });
}

function onToolResult(ev) {
  let ctx = state.toolCards.get(ev.id);
  if (!ctx) {
    // 没见到对应调用（事件丢失等）：补一张只有结果的卡片
    ctx = addToolCard(toolContainerFor(ev.agent), { id: ev.id, name: ev.name, args: '' });
  }
  fillToolOutput(ctx, ev.output ?? '');
}

// ---- 任务 / 用量 / 回复 / 错误 ----

function onTaskStarted(ev) {
  const run = ensureRun();
  ensureTaskCard(run, String(ev.task_id), ev.worker, ev.task);
}

function onTaskFinished(ev) {
  const taskId = String(ev.task_id);
  const ctx = state.runningTasks.get(taskId) || state.run?.tasks.get(taskId);
  if (!ctx) return;
  const card = ctx.el;
  card.classList.remove('running');
  const start = Number(card.dataset.start || Date.now());
  ctx.elapsedEl.textContent = fmtElapsed(start);
  for (const stream of ctx.agentStreams.values()) stream.classList.remove('streaming-cursor');

  if (ev.error === 'cancelled') {
    card.classList.add('cancelled');
    ctx.statusEl.textContent = '⊘';
  } else if (ev.error) {
    card.classList.add('failed');
    ctx.statusEl.textContent = '✗';
    ctx.bodyEl.append(el('div', 'task-result error', ev.error));
  } else {
    card.classList.add('done');
    ctx.statusEl.textContent = '✓';
    if (ev.result) ctx.bodyEl.append(el('div', 'task-result', ev.result));
  }
  state.runningTasks.delete(taskId);
  updateTasksBadge();
  scrollBottom();
}

function onUsage(ev) {
  const run = ensureRun();
  run.usage.prompt += ev.usage.prompt_tokens || 0;
  run.usage.completion += ev.usage.completion_tokens || 0;
  const { prompt, completion } = run.usage;
  run.usageEl.textContent = `tokens ↑ ${prompt} ↓ ${completion}（共 ${prompt + completion}）`;
  run.usageEl.classList.remove('hidden');
}

function onReply(ev) {
  const run = ensureRun();
  // 流式区定型为 markdown 渲染的最终回复
  run.textEl.classList.remove('streaming-cursor');
  run.textEl.classList.add('hidden');
  if (!run.reasonEl.textContent) run.reasonEl.classList.add('hidden');
  const reply = el('div', 'reply-block md');
  reply.append(el('span', 'msg-label', 'thought'));
  const body = el('div');
  body.innerHTML = renderMarkdown(ev.text);
  reply.append(body);
  run.el.insertBefore(reply, run.toolsEl);
  run.el.classList.remove('streaming');
  run.replied = true;
  scrollBottom();
  // 会话列表的 updated_at 变了，静默刷新
  loadSessions();
}

function onErrorEvent(ev) {
  const run = ensureRun();
  run.el.append(el('div', 'run-error', `错误：${ev.message}`));
  scrollBottom();
}

function addNotice(text) {
  const run = ensureRun();
  run.el.append(el('div', 'run-notice', text));
  scrollBottom();
}

function updateTasksBadge() {
  const badge = $('#tasks-badge');
  const n = state.runningTasks.size;
  badge.classList.toggle('hidden', n === 0);
  badge.textContent = n > 0 ? `${n} 个任务运行中` : '';
}

// 运行中任务卡片的耗时每秒刷新
setInterval(() => {
  for (const ctx of state.runningTasks.values()) {
    ctx.elapsedEl.textContent = fmtElapsed(Number(ctx.el.dataset.start || Date.now()));
  }
}, 1000);

// ---- 历史渲染 ----

function renderHistory(history) {
  let runCtx = null; // 当前只读运行块
  for (const m of history) {
    if (m.role === 'user') {
      addUserBlock(m.content || '');
      runCtx = null;
      continue;
    }
    if (m.role === 'assistant') {
      if (!runCtx) {
        const runEl = el('div', 'msg run-block');
        const toolsEl = el('div', 'run-tools');
        runEl.append(toolsEl);
        $('#messages').append(runEl);
        runCtx = { el: runEl, toolsEl };
      }
      if (m.reasoning) {
        const details = el('details', 'history-reasoning');
        details.append(el('summary', null, '思考过程'));
        details.append(el('div', 'reasoning-stream', m.reasoning));
        runCtx.el.insertBefore(details, runCtx.toolsEl);
      }
      if (m.content) {
        const reply = el('div', 'reply-block md');
        reply.append(el('span', 'msg-label', 'thought'));
        const body = el('div');
        body.innerHTML = renderMarkdown(m.content);
        reply.append(body);
        runCtx.el.insertBefore(reply, runCtx.toolsEl);
      }
      for (const tc of m.tool_calls || []) {
        addToolCard(runCtx.toolsEl, { id: tc.id, name: tc.name, args: tc.arguments, readonly: true });
      }
      continue;
    }
    if (m.role === 'tool') {
      const ctx = state.toolCards.get(m.call_id);
      if (ctx) fillToolOutput(ctx, m.content || '');
      else if (runCtx) addToolCard(runCtx.toolsEl, { id: m.call_id, name: 'tool', args: '', output: m.content || '', readonly: true });
    }
  }
  scrollBottom(true);
}

// ---- 发送 ----

async function sendMessage() {
  const input = $('#input');
  const content = input.value.trim();
  if (!content) return;
  if (!state.currentId) {
    await createSession(); // 没有会话时自动建一个
  }
  input.value = '';
  autogrow();
  addUserBlock(content);
  state.run = null; // 新一轮
  ensureRun();
  try {
    await api(`/api/sessions/${encodeURIComponent(state.currentId)}/messages`, {
      method: 'POST',
      body: { content },
    });
  } catch (e) {
    if (e.status !== 401) addNotice(`发送失败：${e.message}`);
  }
}

function autogrow() {
  const input = $('#input');
  input.style.height = 'auto';
  input.style.height = Math.min(input.scrollHeight, 200) + 'px';
}

// ==================== SSE ====================

function setConn(status, text) {
  const dot = $('#conn-dot');
  dot.className = 'conn-dot' + (status !== 'off' ? ` ${status}` : ' off');
  $('#conn-text').textContent = text;
}

function closeSSE() {
  if (state.sse) { state.sse.close(); state.sse = null; }
  if (state.sseTimer) { clearTimeout(state.sseTimer); state.sseTimer = null; }
}

function connectEvents() {
  closeSSE();
  const id = state.currentId;
  if (!id || !state.auth) return;
  setConn('retry', '连接中');
  state.sse = connectSSE(`/api/sessions/${encodeURIComponent(id)}/events`, state.auth.token, {
    onOpen: () => { state.sseRetry = 0; setConn('on', '已连接'); },
    onEvent: (name, data) => {
      state.sseRetry = 0;
      setConn('on', '已连接');
      handleEvent(name, data);
    },
    onClose: (status) => {
      if (status === 401) { logout(); return; }
      setConn('retry', '重连中');
      const delay = Math.min(30000, 1000 * 2 ** state.sseRetry);
      state.sseRetry += 1;
      state.sseTimer = setTimeout(connectEvents, delay);
    },
  });
}

function handleEvent(name, data) {
  if (name === 'lagged') {
    addNotice(`事件流滞后，丢失了 ${data} 条事件`);
    return;
  }
  let ev;
  try {
    ev = JSON.parse(data);
  } catch {
    return;
  }
  switch (name) {
    case 'text_delta': onDelta(ev, false); break;
    case 'reasoning_delta': onDelta(ev, true); break;
    case 'tool_call': onToolCall(ev); break;
    case 'tool_result': onToolResult(ev); break;
    case 'usage': onUsage(ev); break;
    case 'task_started': onTaskStarted(ev); break;
    case 'task_finished': onTaskFinished(ev); break;
    case 'reply': onReply(ev); break;
    case 'error': onErrorEvent(ev); break;
  }
}

// ==================== 记忆视图 ====================

async function loadMemoryView() {
  if (state.memoryLoaded) return;
  state.memoryLoaded = true;
  try {
    const recent = await api('/api/memory?limit=20');
    state.memoryEnabled = true;
    renderMemoryList(recent, false);
    switchMemoryTab('recent');
  } catch (e) {
    if (e.status === 404) {
      state.memoryEnabled = false;
      $('#memory-body').classList.add('hidden');
      $('#memory-disabled').classList.remove('hidden');
    } else {
      $('#memory-list').textContent = '';
      $('#memory-list').append(el('div', 'placeholder', `加载失败：${e.message}`));
    }
  }
}

function switchMemoryTab(tab) {
  state.memoryTab = tab;
  document.querySelectorAll('#view-memory .memory-tabs .tab').forEach((b) => {
    b.classList.toggle('active', b.dataset.mtab === tab);
  });
  $('#memory-search-row').classList.toggle('hidden', tab !== 'search');
  if (tab === 'recent') loadRecentMemory();
}

async function loadRecentMemory() {
  try {
    renderMemoryList(await api('/api/memory?limit=50'), false);
  } catch (e) {
    if (e.status === 404) { state.memoryEnabled = false; $('#memory-body').classList.add('hidden'); $('#memory-disabled').classList.remove('hidden'); }
  }
}

async function searchMemory() {
  const q = $('#memory-q').value.trim();
  if (!q) return;
  try {
    renderMemoryList(await api(`/api/memory/search?q=${encodeURIComponent(q)}&limit=20`), true);
  } catch (e) {
    if (e.status !== 401) {
      $('#memory-list').textContent = '';
      $('#memory-list').append(el('div', 'placeholder', `搜索失败：${e.message}`));
    }
  }
}

function renderMemoryList(items, withScore) {
  const list = $('#memory-list');
  list.textContent = '';
  if (!items.length) {
    list.append(el('div', 'placeholder', withScore ? '没有匹配的记忆' : '暂无记忆'));
    return;
  }
  for (const m of items) {
    const item = el('div', 'memory-item');
    item.append(el('div', 'memory-content', m.content));

    if (withScore && m.relations && m.relations.length) {
      item.append(el('div', 'memory-relations', m.relations.join('  ·  ')));
    }

    const meta = el('div', 'memory-meta');
    if (withScore && typeof m.score === 'number') {
      const bar = el('span', 'score-bar');
      const fill = el('i');
      fill.style.width = `${Math.round(Math.min(1, m.score) * 100)}%`;
      bar.append(fill);
      meta.append(bar, el('span', 'score-num', m.score.toFixed(3)));
      for (const ch of ['semantic', 'sparse', 'graph']) {
        const v = m.channels && m.channels[ch];
        if (typeof v === 'number') {
          const badge = el('span', 'channel-badge');
          badge.append(`${ch} `, Object.assign(el('b'), { textContent: v.toFixed(2) }));
          meta.append(badge);
        }
      }
    }
    for (const t of m.tags || []) meta.append(el('span', 'tag-chip', t));
    meta.append(el('span', null, relTime(m.created_at_ms)));
    const del = el('button', 'memory-del', '删除');
    del.addEventListener('click', () => deleteMemory(m.id, item));
    meta.append(del);
    item.append(meta);
    list.append(item);
  }
}

async function deleteMemory(id, item) {
  if (!confirm('删除这条记忆？')) return;
  await api(`/api/memory/${encodeURIComponent(id)}`, { method: 'DELETE' });
  item.remove();
}

async function writeMemory(e) {
  e.preventDefault();
  const msg = $('#memory-write-msg');
  const content = $('#memory-content').value.trim();
  if (!content) return;
  const tags = $('#memory-tags').value.split(',').map((t) => t.trim()).filter(Boolean);
  try {
    await api('/api/memory', { method: 'POST', body: { content, tags } });
    msg.textContent = '已写入';
    msg.className = 'form-msg';
    $('#memory-content').value = '';
    if (state.memoryTab === 'recent') loadRecentMemory();
  } catch (e2) {
    msg.textContent = `写入失败：${e2.message}`;
    msg.className = 'form-msg err';
  }
  msg.classList.remove('hidden');
}

// ==================== 系统视图 ====================

async function loadSystemView() {
  if (state.systemLoaded) return;
  state.systemLoaded = true;
  const body = $('#system-body');
  body.textContent = '';
  const inner = el('div', 'system-body-inner');
  body.append(inner);

  // agents
  try {
    const agents = await api('/api/agents');
    inner.append(el('div', 'sys-section-title', 'AGENTS'));
    inner.append(agentCard('controller（主控）', { model: agents.controller.model, description: '接收消息、派发任务、汇总结果', tools: [], can_spawn: false }));
    for (const w of agents.workers || []) inner.append(agentCard(w.name, w));
  } catch (e) {
    if (e.status !== 401) inner.append(el('div', 'sys-error', `加载 agents 失败：${e.message}`));
  }

  // models（仅 admin；403 隐藏）
  if (state.auth.user.role === 'admin') {
    try {
      const models = await api('/api/models');
      inner.append(el('div', 'sys-section-title', 'MODELS'));
      inner.append(modelsTable(models));
    } catch (e) {
      if (e.status !== 403 && e.status !== 401) inner.append(el('div', 'sys-error', `加载 models 失败：${e.message}`));
    }
  }
}

function agentCard(name, cfg) {
  const card = el('div', 'agent-card');
  const head = el('div', 'agent-card-head');
  head.append(el('span', 'agent-name', name));
  head.append(el('span', 'agent-model', cfg.model || ''));
  const spawn = el('span', `agent-spawn${cfg.can_spawn ? ' yes' : ''}`, cfg.can_spawn ? '可派生子 agent' : '不可派生');
  head.append(spawn);
  card.append(head);
  if (cfg.description) card.append(el('div', 'agent-desc', cfg.description));
  if (cfg.tools && cfg.tools.length) {
    const tools = el('div', 'agent-tools');
    for (const t of cfg.tools) tools.append(el('span', 'tool-chip', t));
    card.append(tools);
  }
  return card;
}

function modelsTable(models) {
  const table = el('table', 'models-table');
  const thead = el('tr');
  for (const h of ['名称', 'provider', 'model', 'base_url', '参数']) thead.append(el('th', null, h));
  table.append(thead);
  for (const [name, m] of Object.entries(models)) {
    const tr = el('tr');
    tr.append(el('td', 'mono', name));
    tr.append(el('td', 'mono', m.provider || ''));
    tr.append(el('td', 'mono', m.model || ''));
    tr.append(el('td', 'mono', m.base_url || ''));
    const params = ['temperature', 'top_p', 'max_tokens', 'thinking']
      .filter((k) => m[k] !== undefined && m[k] !== null)
      .map((k) => `${k}=${m[k]}`)
      .join(' ');
    tr.append(el('td', 'mono', params || '—'));
    table.append(tr);
  }
  return table;
}

// ==================== 视图切换 / 启动 ====================

function switchView(view) {
  state.view = view;
  document.querySelectorAll('.side-tabs .tab').forEach((b) => {
    b.classList.toggle('active', b.dataset.view === view);
  });
  for (const v of ['chat', 'memory', 'system']) {
    $(`#view-${v}`).classList.toggle('hidden', v !== view);
  }
  $('#app').classList.remove('sidebar-open');
  if (view === 'memory') loadMemoryView();
  if (view === 'system') loadSystemView();
}

function bindEvents() {
  $('#login-form').addEventListener('submit', doLogin);
  $('#btn-logout').addEventListener('click', logout);
  $('#btn-new-session').addEventListener('click', createSession);
  $('#btn-send').addEventListener('click', sendMessage);
  $('#btn-sidebar-toggle').addEventListener('click', () => $('#app').classList.toggle('sidebar-open'));

  document.querySelectorAll('.side-tabs .tab').forEach((b) => {
    b.addEventListener('click', () => switchView(b.dataset.view));
  });

  const input = $('#input');
  input.addEventListener('input', autogrow);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      sendMessage();
    }
  });

  $('#memory-write').addEventListener('submit', writeMemory);
  $('#memory-q').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); searchMemory(); }
  });
  document.querySelectorAll('.memory-tabs .tab').forEach((b) => {
    b.addEventListener('click', () => switchMemoryTab(b.dataset.mtab));
  });

  // 代码块复制按钮（事件委托）
  document.addEventListener('click', async (e) => {
    const btn = e.target.closest('[data-copy]');
    if (!btn) return;
    const code = btn.closest('.codeblock')?.querySelector('code');
    if (!code) return;
    try {
      await navigator.clipboard.writeText(code.textContent);
      btn.textContent = '已复制';
      setTimeout(() => { btn.textContent = '复制'; }, 1500);
    } catch { /* 剪贴板不可用时忽略 */ }
  });
}

async function boot() {
  bindEvents();
  setUnauthorizedHandler(showLogin);
  state.auth = loadAuth();
  if (!state.auth) { showLogin(); return; }
  try {
    // 校验 token 仍然有效
    const me = await api('/api/me');
    state.auth.user = me;
    saveAuth(state.auth);
    showApp();
  } catch {
    showLogin();
  }
}

boot();
