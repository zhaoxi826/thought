// 对话页（主页，浅色书斋）：会话侧栏 + 记忆面板 + 纯净聊天 + 输入框。
// 纯净聊天定调：历史只渲染 user 与 assistant.content；reasoning 默认折叠；
// tool_calls 与 role=tool 一律不显示；不流式渲染 controller 的工作叙述，
// 等 reply 事件到达后渲染最终回复。

import { api, ApiError } from './lib/api.js';
import { renderMarkdown } from './lib/markdown.js';
import { h, fmtClock, fmtRel } from './lib/dom.js';
import { TasksStore } from './tasks-store.js';

let switchPage = () => {};
let focusTask = () => {};

let sessions = [];            // [{id,title,created_at_ms,updated_at_ms}]
let activeId = null;
let unsubscribe = null;       // 退订当前会话事件
let agents = null;            // GET /api/agents 缓存（conv-sub 用）
let turn = null;              // 进行中的一轮：{indicatorEl, workers:Set, taskKeys:[], quietTimer, done}
let memoryEnabled = null;     // null=未知 true/false

export const HomePage = {
  init({ switchPage: sp, focusTask: ft }) {
    switchPage = sp;
    focusTask = ft;
    bindComposer();
    bindSidebar();
  },

  /** 登录/启动后：加载会话列表与 agent 配置。 */
  async start() {
    try { agents = await api('/api/agents'); TasksStore.setAgents(agents); } catch { agents = null; }
    await reloadSessions();
    if (sessions.length) selectSession(sessions[0].id);
    else renderConvHeader();
    TasksStore.start(sessions.map((s) => ({ id: s.id, title: s.title })));
  },

  activeSessionId: () => activeId,
};

// ---------------------------------------------------------------- 侧栏

function bindSidebar() {
  document.getElementById('new-session').addEventListener('click', async () => {
    try {
      const s = await api('/api/sessions', { method: 'POST', body: {} });
      sessions.unshift(s);
      TasksStore.watchSession({ id: s.id, title: s.title });
      renderSessions();
      selectSession(s.id);
    } catch (e) { showError(e.message); }
  });
  bindMemoryPanel();
}

async function reloadSessions() {
  try {
    sessions = await api('/api/sessions');
  } catch { sessions = []; }
  renderSessions();
}

function sessionTitle(s) {
  const t = (s.title || '').trim();
  return !t || t === 'New chat' ? '新会话' : t;
}

function renderSessions() {
  const nav = document.getElementById('sessions');
  nav.textContent = '';
  for (const s of sessions) {
    const item = h('div', {
      class: `session${s.id === activeId ? ' active' : ''}`,
      onclick: () => selectSession(s.id),
    },
      h('div', { class: 't', text: sessionTitle(s) }),
      h('div', { class: 'w', text: fmtRel(s.updated_at_ms) }),
      h('button', {
        class: 'del', type: 'button', title: '删除会话', text: '×',
        onclick(e) { e.stopPropagation(); deleteSession(s); },
      }),
    );
    nav.append(item);
  }
}

async function deleteSession(s) {
  if (!window.confirm(`删除会话「${sessionTitle(s)}」？`)) return;
  try {
    await api(`/api/sessions/${s.id}`, { method: 'DELETE' });
  } catch (e) { showError(e.message); return; }
  sessions = sessions.filter((x) => x.id !== s.id);
  TasksStore.unwatchSession(s.id);
  if (activeId === s.id) {
    activeId = null;
    detachStream();
    clearColumn();
    renderConvHeader();
  }
  renderSessions();
  if (!activeId && sessions.length) selectSession(sessions[0].id);
}

// ---------------------------------------------------------------- 记忆面板

function bindMemoryPanel() {
  const entry = document.getElementById('memory-entry');
  const pop = document.getElementById('memory-pop');
  const search = document.getElementById('memory-search');

  // 点击固定/取消固定（hover 展开由 CSS 负责）；面板内部点击不冒泡
  pop.addEventListener('click', (e) => e.stopPropagation());
  entry.addEventListener('click', () => entry.classList.toggle('pin'));
  entry.addEventListener('mouseenter', () => { if (memoryEnabled === null) loadRecent(); }, { once: false });

  let debounce = null;
  search.addEventListener('input', () => {
    clearTimeout(debounce);
    debounce = setTimeout(() => {
      const q = search.value.trim();
      if (q) searchMemory(q); else loadRecent();
    }, 300);
  });
}

async function loadRecent() {
  const list = document.getElementById('memory-list');
  const title = document.getElementById('memory-pop-title');
  const count = document.getElementById('memory-count');
  title.textContent = '最近记忆';
  try {
    const mems = await api('/api/memory?limit=8');
    memoryEnabled = true;
    count.textContent = '';
    list.textContent = '';
    if (!mems.length) {
      list.append(h('div', { class: 'mem-empty', text: '暂无记忆' }));
      return;
    }
    for (const m of mems) {
      list.append(h('div', { class: 'mem' },
        h('span', { class: 'txt', text: m.content }),
        visChip(m),
        h('span', { class: 'score', text: fmtRel(m.created_at_ms) }),
      ));
    }
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) {
      memoryEnabled = false;
      count.textContent = '未启用';
      list.textContent = '';
      list.append(h('div', { class: 'mem-empty', text: '记忆未启用' }));
    } else {
      list.textContent = '';
      list.append(h('div', { class: 'mem-empty', text: e.message }));
    }
  }
}

/** 可见性小 chip：shared / public 显示，private 不显示。 */
function visChip(m) {
  if (m.visibility === 'shared') return h('span', { class: 'vis', text: '共享' });
  if (m.visibility === 'public') return h('span', { class: 'vis pub', text: '公开' });
  return null;
}

async function searchMemory(q) {
  const list = document.getElementById('memory-list');
  const title = document.getElementById('memory-pop-title');
  title.textContent = '搜索';
  try {
    const hits = await api(`/api/memory/search?q=${encodeURIComponent(q)}&limit=8`);
    memoryEnabled = true;
    list.textContent = '';
    if (!hits.length) {
      list.append(h('div', { class: 'mem-empty', text: '没有命中' }));
      return;
    }
    for (const m of hits) {
      const score = Math.max(0, Math.min(1, m.score ?? 0));
      const barFill = h('i', {});
      barFill.style.width = `${Math.round(score * 100)}%`; // CSSOM 属性设置不受 CSP style-src 限制
      list.append(h('div', { class: 'mem' },
        h('span', { class: 'txt', text: m.content }),
        visChip(m),
        h('span', { class: 'bar' }, barFill),
        h('span', { class: 'score', text: score.toFixed(2) }),
      ));
    }
  } catch (e) {
    list.textContent = '';
    if (e instanceof ApiError && e.status === 404) {
      memoryEnabled = false;
      list.append(h('div', { class: 'mem-empty', text: '记忆未启用' }));
    } else {
      list.append(h('div', { class: 'mem-empty', text: e.message }));
    }
  }
}

// ---------------------------------------------------------------- 会话切换与历史渲染

async function selectSession(id) {
  if (activeId === id) return;
  activeId = id;
  endTurn(false);
  detachStream();
  renderSessions();
  renderConvHeader();
  clearColumn();

  let detail = null;
  try {
    detail = await api(`/api/sessions/${id}`);
  } catch (e) { showError(e.message); return; }
  if (activeId !== id) return; // 等待期间又切走了

  renderHistory(detail.history || []);
  scrollToBottom();

  // 复用适配层那条 SSE（不为活跃会话开第二条）
  unsubscribe = TasksStore.subscribe(id, onSessionEvent);
}

function detachStream() {
  if (unsubscribe) { unsubscribe(); unsubscribe = null; }
}

function renderConvHeader() {
  const title = document.getElementById('conv-title');
  const s = sessions.find((x) => x.id === activeId);
  title.textContent = s ? sessionTitle(s) : 'thought';
  const sub = h('span', {
    class: 'conv-sub',
    text: `主控 controller · ${agents?.controller?.model || '—'}`,
  });
  title.append(sub);
}

function clearColumn() {
  const col = document.getElementById('home-column');
  col.textContent = '';
}

function renderHistory(history) {
  const col = document.getElementById('home-column');
  col.textContent = '';
  for (const msg of history) {
    if (msg.role === 'user') {
      col.append(letter('user', '我', msg.content, null));
    } else if (msg.role === 'assistant') {
      // tool_calls 与 role=tool 一律不显示；reasoning 默认折叠
      if (msg.content) {
        col.append(assistantLetter(msg.content));
      }
      if (msg.reasoning) {
        col.append(reasoningBlock(msg.reasoning));
      }
    }
  }
}

function letter(kind, who, text, when) {
  return h('section', { class: `letter ${kind}` },
    h('div', { class: 'letter-head' },
      h('span', { class: 'who', text: who }),
      when ? h('span', { class: 'when', text: when }) : null,
    ),
    h('div', { class: 'body', text }),
  );
}

function assistantLetter(markdown) {
  return h('section', { class: 'letter assistant' },
    h('div', { class: 'letter-head' },
      h('span', { class: 'who', text: 'thought' }),
      h('span', { class: 'when', text: fmtClock(Date.now()) }),
    ),
    // markdown.js 输出已消毒（输入先整体转义），可用 innerHTML
    h('div', { class: 'body', html: renderMarkdown(markdown) }),
  );
}

function reasoningBlock(text) {
  const box = h('div', { class: 'reasoning' },
    h('button', {
      class: 'r-toggle', type: 'button',
      onclick() { box.classList.toggle('open'); },
    },
      h('span', { class: 'chev', text: '▸' }),
      '思考过程',
    ),
    h('div', { class: 'r-body', text }),
  );
  return box;
}

// ---------------------------------------------------------------- 一轮对话（turn）

function onSessionEvent(name, data) {
  switch (name) {
    case 'task_started':
      if (turn && !turn.done) {
        turn.workers.add(data.worker);
        turn.taskKeys.push(`${activeId}:${data.task_id}`);
        setThinkingText(`已交由 ${[...turn.workers].join('、')} 处理…`);
        armQuietTimer();
      }
      break;
    case 'task_finished':
      if (turn && !turn.done) armQuietTimer(3000);
      break;
    case 'text_delta':
    case 'reasoning_delta':
    case 'tool_call':
    case 'usage':
      // controller 的工作叙述：不流式渲染，只刷新静默计时
      if (turn && !turn.done) armQuietTimer();
      break;
    case 'reply':
      if (turn && !turn.done) finishTurnWithReply(data.text);
      break;
    case 'error':
      if (turn && !turn.done) {
        endTurn(false);
        showError(data.message || '处理出错');
      } else {
        showError(data.message || '处理出错');
      }
      break;
    case 'lagged':
      appendLagNote(data);
      break;
    default: break;
  }
}

function startTurn() {
  endTurn(false);
  const indicator = h('section', { class: 'letter assistant', id: 'thinking-letter' },
    h('div', { class: 'letter-head' }, h('span', { class: 'who', text: 'thought' })),
    h('div', { class: 'thinking' },
      h('span', { class: 'dots' }, h('i', {}), h('i', {}), h('i', {})),
      h('span', { class: 'thinking-text', text: 'thought 正在思考…' }),
    ),
  );
  document.getElementById('home-column').append(indicator);
  scrollToBottom();
  turn = { indicatorEl: indicator, workers: new Set(), taskKeys: [], quietTimer: null, done: false };
  armQuietTimer();
}

function setThinkingText(text) {
  const el = turn?.indicatorEl?.querySelector('.thinking-text');
  if (el) el.textContent = text;
}

/** 静默计时：任务结束后 3s、一般事件后 30s 无 reply，则落下交接行。 */
function armQuietTimer(ms) {
  if (!turn || turn.done) return;
  clearTimeout(turn.quietTimer);
  const wait = ms ?? (turn.workers.size ? 30000 : 120000);
  turn.quietTimer = setTimeout(() => {
    if (!turn || turn.done) return;
    if (turn.workers.size) {
      // 该轮结束仍无 reply：留下 muted 交接小字
      const workers = [...turn.workers].join('、');
      const keys = [...turn.taskKeys];
      const line = h('div', { class: 'handoff' },
        document.createTextNode(`已交由 ${workers} 处理 · `),
        h('a', { onclick: () => { if (keys[0]) focusTask(keys[0]); else switchPage('tasks'); } }, '查看任务 →'),
      );
      turn.indicatorEl.remove();
      document.getElementById('home-column').append(line);
      scrollToBottom();
    } else {
      turn.indicatorEl.remove();
    }
    turn.done = true;
    turn = null;
  }, wait);
}

function finishTurnWithReply(text) {
  const el = turn.indicatorEl;
  clearTimeout(turn.quietTimer);
  const reply = assistantLetter(text || '');
  el.replaceWith(reply);
  scrollToBottom();
  turn.done = true;
  turn = null;
}

function endTurn(scroll) {
  if (!turn) return;
  clearTimeout(turn.quietTimer);
  turn.indicatorEl.remove();
  turn.done = true;
  turn = null;
}

function appendLagNote(n) {
  const col = document.getElementById('home-column');
  col.append(h('div', { class: 'lag-note', text: `连接滞后，已跳过 ${n} 条过程事件` }));
  scrollToBottom();
}

function showError(message) {
  const col = document.getElementById('home-column');
  if (!col) return;
  const bar = h('div', { class: 'err-banner' },
    h('span', { class: 'msg', text: message || '出错了' }),
    h('button', { class: 'x', type: 'button', text: '×', onclick() { bar.remove(); } }),
  );
  col.append(bar);
  scrollToBottom();
}

function scrollToBottom() {
  const sc = document.getElementById('home-scroll');
  sc.scrollTop = sc.scrollHeight;
}

// ---------------------------------------------------------------- 输入框

function bindComposer() {
  const input = document.getElementById('composer-input');
  const send = document.getElementById('composer-send');

  const autogrow = () => {
    input.style.height = 'auto';
    input.style.height = `${Math.min(input.scrollHeight, 140)}px`;
  };
  input.addEventListener('input', autogrow);

  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      doSend();
    }
  });
  send.addEventListener('click', doSend);

  // 代码块复制按钮（事件委托）
  document.getElementById('home-column').addEventListener('click', (e) => {
    const btn = e.target.closest('[data-copy]');
    if (!btn) return;
    const code = btn.closest('.codeblock')?.querySelector('code');
    if (!code) return;
    navigator.clipboard?.writeText(code.textContent).then(() => {
      btn.textContent = '已复制';
      setTimeout(() => { btn.textContent = '复制'; }, 1200);
    });
  });

  async function doSend() {
    const content = input.value.trim();
    if (!content) return;
    input.value = '';
    autogrow();

    // 无会话时发送即先建会话
    if (!activeId) {
      try {
        const s = await api('/api/sessions', { method: 'POST', body: {} });
        sessions.unshift(s);
        TasksStore.watchSession({ id: s.id, title: s.title });
        renderSessions();
        await selectSession(s.id);
      } catch (e) { showError(e.message); return; }
    }
    const sid = activeId;

    document.getElementById('home-column').append(letter('user', '我', content, fmtClock(Date.now())));
    startTurn();
    scrollToBottom();

    try {
      // 等 SSE 就绪再发，避免丢 reply（事件流无回放）
      await TasksStore.waitOpen(sid);
      await api(`/api/sessions/${sid}/messages`, { method: 'POST', body: { content } });
    } catch (e) {
      endTurn(false);
      showError(e.message);
    }
  }
}
