// 会话消息区的渲染原语：运行块、任务卡片、工具卡片、回复块（实时事件与历史共用）。

import { state } from '../state.js';
import { $, el, button, fmtElapsed } from '../util.js';
import { renderMarkdown } from '../lib/markdown.js';

// 消息区接近底部时才跟随滚动，避免打断用户翻历史
export function scrollBottom(force = false) {
  const box = $('#messages');
  const nearBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 160;
  if (force || nearBottom) box.scrollTop = box.scrollHeight;
}

export function clearMessages(placeholder) {
  const box = $('#messages');
  box.textContent = '';
  if (placeholder) box.append(el('div', 'placeholder', placeholder));
}

export function removePlaceholder() {
  $('#messages .placeholder')?.remove();
}

/** 清空会话内的渲染上下文（切换会话 / 重新拉历史前调用）。 */
export function resetRenderState() {
  state.run = null;
  state.toolCards.clear();
  state.taskCards.clear();
  state.runningTasks.clear();
  updateTasksBadge();
}

export function addUserBlock(content) {
  removePlaceholder();
  const msg = el('div', 'msg msg-user');
  msg.append(el('span', 'msg-label', '你'));
  msg.append(el('span', null, content));
  $('#messages').append(msg);
  scrollBottom();
}

/** markdown 渲染的回复块（实时 reply 与历史共用）。 */
export function replyBlock(text) {
  const reply = el('div', 'reply-block md');
  reply.append(el('span', 'msg-label', 'thought'));
  const body = el('div');
  body.innerHTML = renderMarkdown(text);
  reply.append(body);
  return reply;
}

/** 可折叠卡片的头部按钮（键盘可达，aria-expanded 同步状态）。 */
function toggleHead(className, card, startOpen) {
  const head = button(className);
  head.setAttribute('aria-expanded', String(startOpen));
  head.addEventListener('click', () => {
    const closed = card.classList.toggle('closed');
    head.setAttribute('aria-expanded', String(!closed));
  });
  return head;
}

// ---- 运行块 ----

/** 当前运行块；没有或已完成（收到 reply）时新建一个。 */
export function ensureRun() {
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
    done: false,
  };
  return state.run;
}

/** 附加类信息（用量/错误/提示）挂到最近的运行块上，不为此新开一块。 */
export function latestRun() {
  return state.run || ensureRun();
}

// agent 标签：'controller' | 'worker#taskId' | 'worker#taskId/subN'
export function parseAgent(label) {
  if (!label || label === 'controller') return null;
  const head = label.split('/')[0];
  const [worker, taskId] = head.split('#');
  return { worker, taskId: taskId || '' };
}

/** 任务卡片：task_started 或首个属于该任务的事件触发创建；同一会话内按 task_id 复用。 */
export function ensureTaskCard(taskId, worker, taskText) {
  const existing = state.taskCards.get(taskId);
  if (existing) return existing;
  const run = ensureRun();

  const card = el('div', 'task-card running');
  card.dataset.start = String(Date.now());
  const head = toggleHead('task-head', card, true);
  const status = el('span', 'task-status');
  status.setAttribute('aria-label', '运行中');
  head.append(status, el('span', 'task-worker', `${worker}#${taskId}`));
  head.append(el('span', 'task-text', taskText || ''));
  const elapsed = el('span', 'task-elapsed', '0s');
  const toggle = el('span', 'task-toggle', '▼');
  toggle.setAttribute('aria-hidden', 'true');
  head.append(elapsed, toggle);
  const body = el('div', 'task-body');
  card.append(head, body);
  run.tasksEl.append(card);
  scrollBottom();

  const ctx = { el: card, bodyEl: body, statusEl: status, elapsedEl: elapsed, agentStreams: new Map() };
  state.taskCards.set(taskId, ctx);
  state.runningTasks.set(taskId, ctx);
  updateTasksBadge();
  return ctx;
}

export function taskFor(loc) {
  return ensureTaskCard(loc.taskId, loc.worker);
}

// ---- 工具调用卡片 ----

export function toolContainerFor(agent) {
  const loc = parseAgent(agent);
  return loc ? taskFor(loc).bodyEl : ensureRun().toolsEl;
}

function prettyArgs(raw) {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

export function addToolCard(container, { id, name, args, output, readonly }) {
  const card = el('div', 'tool-card closed' + (output === undefined ? ' pending' : ' done'));
  const head = toggleHead('tool-head', card, false);
  const status = el('span', 'tool-status', output === undefined ? '⏳' : '✓');
  status.setAttribute('aria-label', output === undefined ? '执行中' : '完成');
  head.append(status);
  head.append(el('span', 'tool-name', name || ''));
  const summary = el('span', 'tool-summary', (args || '').slice(0, 120));
  const toggle = el('span', 'tool-toggle', '▼');
  toggle.setAttribute('aria-hidden', 'true');
  head.append(summary, toggle);

  const detail = el('div', 'tool-detail');
  if (args) {
    detail.append(el('div', 'tool-section-label', '参数'));
    detail.append(el('pre', null, prettyArgs(args)));
  }
  const outWrap = el('div', 'tool-output-wrap collapsed hidden');
  const outPre = el('pre');
  const expandBtn = button('tool-expand', '展开全部');
  expandBtn.setAttribute('aria-expanded', 'false');
  expandBtn.addEventListener('click', () => {
    const collapsed = outWrap.classList.toggle('collapsed');
    expandBtn.textContent = collapsed ? '展开全部' : '收起';
    expandBtn.setAttribute('aria-expanded', String(!collapsed));
  });
  outWrap.append(outPre, expandBtn);
  detail.append(outWrap);
  card.append(head, detail);
  container.append(card);
  scrollBottom();

  const ctx = { el: card, statusEl: status, outWrap, outPre, readonly };
  state.toolCards.set(id, ctx);
  if (output !== undefined) fillToolOutput(ctx, output);
  return ctx;
}

export function fillToolOutput(ctx, output) {
  ctx.el.classList.remove('pending');
  ctx.el.classList.add('done');
  ctx.statusEl.textContent = '✓';
  ctx.statusEl.setAttribute('aria-label', '完成');
  ctx.outPre.textContent = output;
  ctx.outWrap.classList.remove('hidden');
  scrollBottom();
}

// ---- 运行块附加信息 ----

export function addNotice(text) {
  latestRun().el.append(el('div', 'run-notice', text));
  scrollBottom();
}

export function updateTasksBadge() {
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
