// 对话页（主页，浅色书斋）：她的状态侧栏 + 和她的对话 + 输入框。
// 心智接口（docs/mind-api.md）：没有会话，只有一条和她的对话。
//   - GET  /api/mind/conversation?after=   历史（升序，按 seq 分页补齐）
//   - GET  /api/mind/conversation/events   SSE：moment / thinking / lagged；连着即"在线"
//   - POST /api/mind/say                   202，返回自己这句话的 Moment；她的回应走事件流
// 只渲染 heard（我说的）与 said（她对我说的）；以 seq 去重、排序。
// 她不一定每句都回，也可能晚点回：等待指示只在"我说了话且她在想"时出现。

import { api, ApiError, loadAuth, clearAuth } from './lib/api.js';
import { connectSSE } from './lib/sse.js';
import { renderMarkdown } from './lib/markdown.js';
import { h, fmtClock } from './lib/dom.js';
import { TasksStore } from './tasks-store.js';
import { HerPanel } from './home-her.js';
import { MemoryPanel } from './home-memory.js';
import { InnerView } from './home-inner.js';

const PAGE = 500;
const QUIET_GRACE = 1500; // 想完后再等一会儿：回复可能紧随 thinking=false 到达

let unauthorized = () => {};

const conv = {
  moments: [],          // 已渲染的 heard / said，按 seq 升序
  seen: new Set(),      // seq 去重（含回显、补齐）
  lastSeq: 0,           // 已知最大 seq（含不渲染的）
  enabled: null,        // null=未知 true/false（404 = 心智未启用）
  thinking: false,
  waiting: false,       // 我说了话，还没等到她的回应
  quietTimer: null,
  stream: null,
  retry: { timer: null, delay: 1000 },
  syncing: false,
  resync: false,
  connected: false,
};

export const HomePage = {
  init({ focusTask, onUnauthorized }) {
    unauthorized = onUnauthorized || (() => {});
    bindComposer();
    MemoryPanel.init();
    HerPanel.init({ focusTask });
    InnerView.init({ onUnauthorized: () => unauthorized() });
  },

  /** 登录/启动后：拉历史、连事件流、拉她的状态。可重入（重新登录）。 */
  async start() {
    try { TasksStore.setAgents(await api('/api/agents')); } catch { /* 非 admin 或未配置 */ }
    TasksStore.start();
    reset();
    InnerView.start(loadAuth()?.user?.role === 'admin');
    HerPanel.start();
    await catchUp();
    if (conv.enabled !== false) connect();
  },
};

function reset() {
  disconnect();
  clearTimeout(conv.quietTimer);
  Object.assign(conv, {
    moments: [], seen: new Set(), lastSeq: 0, enabled: null,
    thinking: false, waiting: false, quietTimer: null, syncing: false, resync: false, connected: false,
  });
  conv.retry.delay = 1000;
  document.getElementById('home-column').textContent = '';
  renderHeader();
}

// ---------------------------------------------------------------- 历史与补齐

/** 从 lastSeq 往后补齐（打开页面、重连、lagged 时）。并发调用合并成一次追加。 */
async function catchUp() {
  if (conv.syncing) { conv.resync = true; return; }
  conv.syncing = true;
  try {
    do {
      conv.resync = false;
      for (;;) {
        const page = await api(`/api/mind/conversation?after=${conv.lastSeq}&limit=${PAGE}`);
        setEnabled(true);
        for (const m of page.moments || []) add(m, false);
        if ((page.moments || []).length < PAGE) break;
      }
    } while (conv.resync);
    renderAll();
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) setEnabled(false);
    else if (!(e instanceof ApiError && e.status === 401)) showError(e.message);
  } finally {
    conv.syncing = false;
  }
}

/** 收一个 Moment。live=true 时按需增量渲染。 */
function add(m, live) {
  if (!m || typeof m.seq !== 'number' || conv.seen.has(m.seq)) return;
  conv.seen.add(m.seq);
  conv.lastSeq = Math.max(conv.lastSeq, m.seq);
  const kind = m.percept?.kind || m.expressed?.kind;
  if (kind !== 'heard' && kind !== 'said') return;

  const tail = conv.moments[conv.moments.length - 1];
  if (!tail || tail.seq < m.seq) {
    conv.moments.push(m);
    if (live) appendMoment(m, tail);
  } else {
    // 乱序（补齐的比实时的早）：插入后整列重绘
    const i = conv.moments.findIndex((x) => x.seq > m.seq);
    conv.moments.splice(i, 0, m);
    if (live) renderAll();
  }
  if (kind === 'said') settle();
}

// ---------------------------------------------------------------- 渲染

function renderAll() {
  const col = document.getElementById('home-column');
  col.textContent = '';
  if (conv.enabled === false) {
    col.append(h('div', { class: 'conv-empty' },
      h('p', { text: '心智未启用。' }),
      h('p', { class: 'hint', text: '在 thought.toml 里加上 [mind] 段并重启后，就可以和她说话了。' }),
    ));
    return;
  }
  col.append(h('div', { class: 'conv-notice', text: '你和思想的对话对管理员可见' }));
  if (!conv.moments.length) {
    col.append(h('div', { class: 'conv-empty' },
      h('p', { text: '还没有说过话。' }),
      h('p', { class: 'hint', text: '她不一定每句都回，也可能晚点再回——她有自己的事。' }),
    ));
  }
  let prev = null;
  for (const m of conv.moments) { col.append(...momentNodes(m, prev)); prev = m; }
  renderWaiting();
  scrollToBottom(true);
}

function appendMoment(m, prev) {
  const col = document.getElementById('home-column');
  col.querySelector('.conv-empty')?.remove();
  const stick = nearBottom();
  const ind = document.getElementById('thinking-letter');
  for (const n of momentNodes(m, prev)) col.insertBefore(n, ind || null);
  scrollToBottom(stick);
}

/** 一条时刻（必要时前面带日期分隔）。 */
function momentNodes(m, prev) {
  const out = [];
  if (!prev || dayKey(prev.at_ms) !== dayKey(m.at_ms)) out.push(h('div', { class: 'day-sep' }, h('span', { text: fmtDay(m.at_ms) })));
  if (m.percept?.kind === 'heard') {
    out.push(letter('user', '我', h('div', { class: 'body', text: m.percept.text }), m.at_ms));
  } else {
    // markdown.js 输出已消毒（输入先整体转义），可用 innerHTML
    out.push(letter('assistant', '思想', h('div', { class: 'body', html: renderMarkdown(m.expressed.text || '') }), m.at_ms));
  }
  return out;
}

function letter(kind, who, body, at) {
  return h('section', { class: `letter ${kind}` },
    h('div', { class: 'letter-head' },
      h('span', { class: 'who', text: who }),
      h('span', { class: 'when', text: fmtClock(at) }),
    ),
    body,
  );
}

function dayKey(ms) {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

function fmtDay(ms) {
  const d = new Date(ms);
  const today = new Date();
  const yest = new Date(today.getFullYear(), today.getMonth(), today.getDate() - 1);
  if (dayKey(ms) === dayKey(today.getTime())) return '今天';
  if (dayKey(ms) === dayKey(yest.getTime())) return '昨天';
  const md = `${d.getMonth() + 1} 月 ${d.getDate()} 日`;
  return d.getFullYear() === today.getFullYear() ? md : `${d.getFullYear()} 年 ${md}`;
}

// ---------------------------------------------------------------- 她在想 / 等待

function setThinking(active) {
  conv.thinking = active;
  TasksStore.setMind({ thinking: active });
  HerPanel.setThinking(active);
  renderHeader();
  if (active) {
    clearTimeout(conv.quietTimer);
    conv.quietTimer = null;
  } else if (conv.waiting) {
    // 想完了还没回我：稍等片刻（回复可能紧随其后），仍没有就如实说明
    clearTimeout(conv.quietTimer);
    conv.quietTimer = setTimeout(() => {
      conv.quietTimer = null;
      if (!conv.waiting || conv.thinking) return;
      conv.waiting = false;
      renderWaiting();
      const col = document.getElementById('home-column');
      col.append(h('div', { class: 'handoff quiet', text: '她听到了，此刻没有回话。' }));
      scrollToBottom(nearBottom());
    }, QUIET_GRACE);
  }
  renderWaiting();
}

/** 她对我开口了：结束等待，去掉"没有回话"的小字。 */
function settle() {
  conv.waiting = false;
  clearTimeout(conv.quietTimer);
  conv.quietTimer = null;
  document.querySelectorAll('#home-column .handoff.quiet').forEach((n) => n.remove());
  renderWaiting();
}

/** 等待指示：我在等，且她正在想。 */
function renderWaiting() {
  const col = document.getElementById('home-column');
  let ind = document.getElementById('thinking-letter');
  const show = conv.waiting && conv.thinking;
  if (!show) { ind?.remove(); return; }
  if (ind) return;
  ind = h('section', { class: 'letter assistant', id: 'thinking-letter' },
    h('div', { class: 'letter-head' }, h('span', { class: 'who', text: '思想' })),
    h('div', { class: 'thinking' },
      h('span', { class: 'dots' }, h('i', {}), h('i', {}), h('i', {})),
      h('span', { class: 'thinking-text', text: '她在想…' }),
    ),
  );
  const stick = nearBottom();
  col.append(ind);
  scrollToBottom(stick);
}

function renderHeader() {
  const title = document.getElementById('conv-title');
  title.textContent = '思想';
  let sub;
  if (conv.enabled === false) sub = '未启用';
  else if (!conv.connected) sub = conv.enabled === null ? '' : '连接中…';
  else sub = conv.thinking ? '在想…' : '在';
  title.append(h('span', { class: `conv-sub${conv.thinking ? ' live' : ''}`, text: sub }));
}

function setEnabled(on) {
  if (conv.enabled === on) return;
  conv.enabled = on;
  TasksStore.setMind({ enabled: on });
  const input = document.getElementById('composer-input');
  input.disabled = !on;
  input.placeholder = on ? '和她说点什么' : '心智未启用';
  document.getElementById('composer-send').classList.toggle('disabled', !on);
  renderHeader();
  if (!on) renderAll();
}

// ---------------------------------------------------------------- 事件流

/** 指数退避 1s→30s；401 回登录；404 = 心智未启用，不重连。连上后用 lastSeq 补齐。 */
function connect() {
  if (conv.stream || conv.retry.timer) return;
  const auth = loadAuth();
  if (!auth) return;
  conv.stream = connectSSE('/api/mind/conversation/events', auth.token, {
    onOpen() {
      conv.connected = true;
      conv.retry.delay = 1000;
      renderHeader();
      catchUp();
    },
    onEvent(name, raw) {
      if (name === 'moment') {
        let m; try { m = JSON.parse(raw); } catch { return; }
        add(m, true);
      } else if (name === 'thinking') {
        setThinking(raw.trim() === 'true');
      } else if (name === 'lagged') {
        catchUp();
      }
    },
    onClose(status) {
      conv.stream = null;
      conv.connected = false;
      if (conv.thinking) setThinking(false);
      renderHeader();
      if (status === 401) { clearAuth(); unauthorized(); return; }
      if (status === 404) { setEnabled(false); return; }
      conv.retry.timer = setTimeout(() => { conv.retry.timer = null; connect(); }, conv.retry.delay);
      conv.retry.delay = Math.min(conv.retry.delay * 2, 30000);
    },
  });
}

function disconnect() {
  clearTimeout(conv.retry.timer);
  conv.retry.timer = null;
  if (conv.stream) { conv.stream.close(); conv.stream = null; }
  conv.connected = false;
}

// ---------------------------------------------------------------- 提示与滚动

function showError(message) {
  const col = document.getElementById('home-column');
  const bar = h('div', { class: 'err-banner', role: 'alert' },
    h('span', { class: 'msg', text: message || '出错了' }),
    h('button', { class: 'x', type: 'button', text: '×', 'aria-label': '关闭', onclick() { bar.remove(); } }),
  );
  col.append(bar);
  scrollToBottom(true);
}

function nearBottom() {
  const sc = document.getElementById('home-scroll');
  return sc.scrollHeight - sc.scrollTop - sc.clientHeight < 120;
}

/** 只在用户本来就在底部附近（或强制）时跟随到底，不打断往上翻历史。 */
function scrollToBottom(force) {
  if (!force) return;
  const sc = document.getElementById('home-scroll');
  sc.scrollTop = sc.scrollHeight;
}

// ---------------------------------------------------------------- 输入框

function bindComposer() {
  const input = document.getElementById('composer-input');
  const send = document.getElementById('composer-send');
  let sending = false;

  const autogrow = () => {
    input.style.height = 'auto';
    input.style.height = `${Math.min(input.scrollHeight, 140)}px`;
  };
  input.addEventListener('input', autogrow);
  input.addEventListener('keydown', (e) => {
    // 输入法组字中的回车不发送
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      doSend();
    }
  });
  send.addEventListener('click', doSend);
  send.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); doSend(); }
  });

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
    const text = input.value.trim();
    if (!text || sending || conv.enabled === false) return;
    sending = true;
    send.classList.add('busy');
    try {
      const m = await api('/api/mind/say', { method: 'POST', body: { text } });
      input.value = '';
      autogrow();
      setEnabled(true);
      add(m, true);
      conv.waiting = true;
      document.querySelectorAll('#home-column .handoff.quiet').forEach((n) => n.remove());
      renderWaiting();
      scrollToBottom(true);
      if (!conv.stream && !conv.retry.timer) connect();
    } catch (e) {
      // 失败时保留输入，方便重发
      if (e instanceof ApiError && e.status === 429) {
        const secs = /(\d+)\s*s/.exec(e.message)?.[1];
        showError(secs ? `说得太快了，${secs} 秒后再试` : '说得太快了，稍后再试');
      } else if (e instanceof ApiError && e.status === 404) {
        setEnabled(false);
      } else if (!(e instanceof ApiError && e.status === 401)) {
        showError(e.message);
      }
    } finally {
      sending = false;
      send.classList.remove('busy');
      input.focus();
    }
  }
}
