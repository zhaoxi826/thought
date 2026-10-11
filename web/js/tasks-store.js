// 任务数据适配层（adapter）—— 后端任务注册表版（design/backend-api.md §1）。
//
// 数据源：
//   - GET  /api/tasks?status=&limit=&cursor=   列表（含历史，游标分页，「加载更多」）
//   - GET  /api/tasks/events                    全局任务事件流：一条 SSE，data 带 session_id
//   - GET  /api/tasks/{sid}/{tid}               详情：持久化 events 回填活动流
//   - POST /api/tasks/{sid}/{tid}/cancel        取消
// 逐会话 SSE（GET /api/sessions/{id}/events）只为对话页保留：对话页订阅时才为该会话建一条，
// 承载 reply / error 等 controller 事件。任务聚合只吃全局流，两条流不会重复计数。
//
// 活动流合并：每个任务保存两份原始记录 —— persisted（详情接口回填）与 log（页面存活期内
// 全局流收到的事件），渲染时（events getter）按 persisted → 去重后的 log 重放成 UI 结构。
// 运行中 agent 段的文本可能尚未落库，由 log 补齐；落库后按子串去重。
//
// 对外接口：
//   start(sessions)               登录后启动：sessions = [{id,title}]
//   watchSession(s) / unwatchSession(id)   会话标题登记 / 会话删除后移除其任务
//   subscribe(id, fn)             对话页订阅会话事件（按需建逐会话 SSE），返回退订函数
//   waitOpen(id)                  等该会话 SSE 进入 open（发消息前调用，避免丢 reply）
//   list() / get(key) / runningCount() / onChange(fn)
//   loadView(view) / viewKeys(view) / viewState(view)   按筛选分页（tab 直接映射 status 参数）
//   ensureDetail(key, force)      回填持久化活动事件（幂等；force 时失败可重试）
//   loadKey(key)                  深链：按 key 拉详情建出任务
//   cancel(key)                   取消任务；失败抛 ApiError（404 / 409）
//   agents / setAgents(a) / limits             GET /api/agents 缓存（含 limits）

import { api, loadAuth, clearAuth } from './lib/api.js';
import { connectSSE } from './lib/sse.js';

const AGENT_RE = /^([^#]+)#(\d+)(\/sub\d+)?$/; // "coder#3" / "coder#3/sub1"
const PAGE = 50;
const TASK_EVENTS = new Set(['task_started', 'task_finished', 'tool_call', 'tool_result', 'text_delta', 'usage']);
const TERMINAL = new Set(['done', 'error', 'cancelled']);
/** 列表筛选 tab → status 查询参数。 */
const VIEW_STATUS = { all: '', running: 'running', done: 'done', failed: 'error,cancelled' };

const tasks = new Map();          // `${sessionId}:${taskId}` -> task
const titles = new Map();         // sessionId -> 会话标题
const streams = new Map();        // sessionId -> 对话页逐会话 SSE 状态
const changeListeners = new Set();
const pendingStubs = new Set();   // 初始列表加载期间由事件临时建出的任务
const globalStream = { conn: null, timer: null, retryDelay: 1000, everOpened: false };
const views = {};                 // filter -> { keys:[], cursor, loading, exhausted, started }
let agents = null;                // { controller, workers, limits }
let unauthorized = () => {};
let initialLoading = false;

export const TasksStore = {
  get agents() { return agents; },
  get limits() { return agents?.limits || null; },
  setAgents(a) { agents = a; emitChange(); },
  setUnauthorizedHandler(fn) { unauthorized = fn; },

  onChange(fn) { changeListeners.add(fn); return () => changeListeners.delete(fn); },

  list() {
    return [...tasks.values()].sort((a, b) => {
      if ((a.status === 'running') !== (b.status === 'running')) return a.status === 'running' ? -1 : 1;
      return (b.startedAt || 0) - (a.startedAt || 0);
    });
  },
  get(k) { return tasks.get(k); },
  runningCount() {
    let n = 0;
    for (const t of tasks.values()) if (t.status === 'running') n++;
    return n;
  },

  /** 登录后启动：先连全局流再拉列表，列表加载期间到达的事件不会丢。可重入（重新登录）。 */
  async start(sessionList) {
    for (const s of sessionList) titles.set(s.id, s.title);
    tasks.clear();
    for (const k of Object.keys(views)) delete views[k];
    initialLoading = true;
    openGlobalStream();
    try {
      const [, running] = await Promise.all([
        this.loadView('all'),
        // 长时间运行的任务可能排在第一页之外，单独补齐全部进行中
        api('/api/tasks?status=running&limit=200').catch(() => null),
      ]);
      // 运行中任务提最前（设计稿如此）：补进来的插入各视图头部
      for (const s of (running?.tasks || []).slice().reverse()) {
        upsertSummary(s);
        const k = key(s.session_id, s.task_id);
        for (const v of ['all', 'running']) {
          const vw = views[v];
          if (vw && !vw.keys.includes(k)) vw.keys.unshift(k);
        }
      }
    } catch (e) {
      console.error('任务列表加载失败', e);
    }
    initialLoading = false;
    for (const t of pendingStubs) if (!t.summaryKnown) loadDetail(t);
    pendingStubs.clear();
    emitChange();
  },

  /** 加载某筛选视图的下一页（首次调用拉第一页）。返回本页任务。 */
  async loadView(view) {
    const st = views[view] ??= { keys: [], cursor: null, loading: false, exhausted: false, started: false };
    if (st.loading || st.exhausted) return [];
    st.loading = true;
    emitChange();
    try {
      const status = VIEW_STATUS[view] || '';
      const q = `?limit=${PAGE}${status ? `&status=${status}` : ''}${st.cursor ? `&cursor=${encodeURIComponent(st.cursor)}` : ''}`;
      const page = await api(`/api/tasks${q}`);
      const items = page?.tasks || [];
      for (const s of items) {
        upsertSummary(s);
        const k = key(s.session_id, s.task_id);
        if (!st.keys.includes(k)) st.keys.push(k);
      }
      st.cursor = page?.next_cursor ?? null;
      st.exhausted = st.cursor == null;
      st.started = true;
      return items;
    } finally {
      st.loading = false;
      emitChange();
    }
  },

  viewKeys(view) { return views[view]?.keys || []; },
  viewState(view) {
    const st = views[view];
    return { hasMore: !!st && !st.exhausted, loading: !!st?.loading, started: !!st?.started };
  },

  /** 深链：按 key 拉详情建出任务（任务不在已加载列表里时用）。 */
  async loadKey(k) {
    const [sid, tid] = k.split(':');
    if (!sid || tid == null) return;
    const t = ensureTask(sid, Number(tid), null);
    await loadDetail(t);
  },

  ensureDetail(k, force = false) {
    const t = tasks.get(k);
    if (!t) return;
    if (t.detail === 'none' || t.detail === 'stale' || (force && t.detail === 'error')) loadDetail(t);
  },

  async cancel(k) {
    const t = tasks.get(k);
    if (!t) return;
    try {
      await api(`/api/tasks/${encodeURIComponent(t.sessionId)}/${t.taskId}/cancel`, { method: 'POST' });
    } catch (e) {
      // 409：任务其实已结束（可能漏了 task_finished），拉详情纠正状态
      if (e?.status === 409) loadDetail(t);
      throw e;
    }
  },

  watchSession(s) { titles.set(s.id, s.title); },
  unwatchSession(id) {
    closeSessionStream(id);
    titles.delete(id);
    for (const k of [...tasks.keys()]) if (k.startsWith(`${id}:`)) tasks.delete(k);
    emitChange();
  },

  /** 对话页订阅：按需建立该会话的 SSE；没有订阅者的会话流随即关闭。返回退订函数。 */
  subscribe(id, fn) {
    const s = sessionStream(id);
    s.listeners.add(fn);
    pruneSessionStreams();
    return () => { s.listeners.delete(fn); pruneSessionStreams(); };
  },

  /** 等 SSE 进入 open；已在 open 立即返回。失败/超时（8s）也返回，不阻塞发送。 */
  waitOpen(id) {
    const s = sessionStream(id);
    if (s.state === 'open') return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setTimeout(resolve, 8000);
      s.openWaiters.push(() => { clearTimeout(timer); resolve(); });
    });
  },
};

/** worker chip 用：某 worker 当前运行中的任务。 */
export function runningByWorker() {
  const map = new Map(); // worker -> [task]
  for (const t of tasks.values()) {
    if (t.status !== 'running') continue;
    if (!map.has(t.worker)) map.set(t.worker, []);
    map.get(t.worker).push(t);
  }
  return map;
}

/** 并发仪表用：进行中任务最多的那个会话的进行中数（并发上限按会话计）。 */
export function busiestSessionRunning() {
  const per = new Map();
  for (const t of tasks.values()) if (t.status === 'running') per.set(t.sessionId, (per.get(t.sessionId) || 0) + 1);
  return per.size ? Math.max(...per.values()) : 0;
}

/** controller 是否忙碌。只有对话页订阅中的会话有逐会话流，故只反映当前会话。 */
export function controllerBusy() {
  for (const s of streams.values()) if (s.controllerBusy) return true;
  return false;
}

// ---------------------------------------------------------------- 内部：任务对象

function emitChange() {
  for (const fn of changeListeners) {
    try { fn(); } catch (e) { console.error('TasksStore 监听者出错', e); }
  }
}

function key(sessionId, taskId) { return `${sessionId}:${taskId}`; }
function callKey(agent, id) { return `${agent}|${id}`; }

function ensureTask(sessionId, taskId, worker) {
  const k = key(sessionId, taskId);
  let t = tasks.get(k);
  if (t) return t;
  t = {
    key: k, sessionId, taskId: Number(taskId),
    worker: worker || '?', label: `${worker || '?'}#${taskId}`,
    get sessionTitle() { return titles.get(this.sessionId) || ''; },
    task: '', status: 'running',
    startedAt: Date.now(), finishedAt: null, result: null, error: null,
    tokens: 0, baseTokens: 0, liveTokens: 0,
    summaryKnown: false,
    detail: 'none',               // none | loading | loaded | stale | error | missing
    persisted: [], log: [],       // 原始记录：详情回填 / 实时事件
    dirty: true, view: null,      // 渲染结构缓存（events/calls/subs）
    get events() { return viewOf(this).events; },
    get subs() { return viewOf(this).subs; },
  };
  tasks.set(k, t);
  return t;
}

/** 事件先于列表 / 详情到达的未知任务：建占位，随后拉详情补齐摘要。 */
function stub(sessionId, taskId, worker) {
  const t = ensureTask(sessionId, taskId, worker);
  if (!t.summaryKnown) {
    if (initialLoading) pendingStubs.add(t);
    else loadDetail(t);
  }
  return t;
}

function upsertSummary(s) {
  const t = ensureTask(s.session_id, s.task_id, s.worker);
  applySummary(t, s);
  return t;
}

function applySummary(t, s) {
  if (s.worker) { t.worker = s.worker; t.label = `${s.worker}#${t.taskId}`; }
  if (s.task != null) t.task = s.task;
  if (s.started_at_ms) t.startedAt = s.started_at_ms;
  t.summaryKnown = true;
  // 本地已收到结束事件而快照仍是 running（快照更旧）：终态不回退
  if (TERMINAL.has(t.status) && s.status === 'running') return;
  t.status = s.status || t.status;
  t.finishedAt = s.finished_at_ms ?? (t.status === 'running' ? null : t.finishedAt || Date.now());
  t.result = s.result ?? null;
  t.error = s.error ?? null;
  t.baseTokens = s.usage?.total_tokens || 0;
  t.liveTokens = 0;
  t.tokens = t.baseTokens;
  t.dirty = true;
}

async function loadDetail(t) {
  if (t.detail === 'loading') return;
  t.detail = 'loading';
  emitChange();
  try {
    const d = await api(`/api/tasks/${encodeURIComponent(t.sessionId)}/${t.taskId}`);
    applySummary(t, d);
    t.persisted = (d.events || []).map(fromPersisted).filter(Boolean);
    // 快照仍在运行但本地已结束：最后一段可能未落库，下次选中时再拉
    t.detail = TERMINAL.has(t.status) && d.status === 'running' ? 'stale' : 'loaded';
  } catch (e) {
    t.detail = e?.status === 404 ? 'missing' : 'error';
  }
  t.dirty = true;
  emitChange();
}

function fromPersisted(e) {
  const ts = e.ts_ms || Date.now();
  switch (e.kind) {
    case 'text': return { kind: 'text', agent: e.agent, text: e.text || '', ts };
    case 'tool_call': return { kind: 'call', agent: e.agent, id: e.id, name: e.name, arguments: e.arguments, ts };
    case 'tool_result': return { kind: 'result', agent: e.agent, id: e.id, name: e.name, output: e.output, ts };
    default: return null; // usage：用量以任务汇总 usage 为准
  }
}

// ---------------------------------------------------------------- 内部：全局任务事件流

/** 全局流：指数退避 1s→30s；401 全局回登录；重连 / lagged 后重新对账。 */
function openGlobalStream() {
  if (globalStream.conn || globalStream.timer) return;
  const auth = loadAuth();
  if (!auth) return;
  globalStream.conn = connectSSE('/api/tasks/events', auth.token, {
    onOpen() {
      const reconnect = globalStream.everOpened;
      globalStream.everOpened = true;
      globalStream.retryDelay = 1000;
      if (reconnect) resync();
    },
    onEvent(name, raw) {
      if (name === 'lagged') { resync(); return; } // 丢了事件且无回放：按注册表对账
      if (!TASK_EVENTS.has(name)) return;
      let data;
      try { data = JSON.parse(raw); } catch { return; }
      handleEvent(name, data);
    },
    onClose(status) {
      globalStream.conn = null;
      if (status === 401) { clearAuth(); unauthorized(); return; }
      globalStream.timer = setTimeout(() => { globalStream.timer = null; openGlobalStream(); }, globalStream.retryDelay);
      globalStream.retryDelay = Math.min(globalStream.retryDelay * 2, 30000);
    },
  });
}

/** 断线 / 滞后后对账：刷新进行中与第一页；本地仍 running 而服务端已不是的，拉详情纠正。 */
async function resync() {
  try {
    const [running, page] = await Promise.all([
      api('/api/tasks?status=running&limit=200'),
      api(`/api/tasks?limit=${PAGE}`),
    ]);
    const live = new Set();
    for (const s of running?.tasks || []) { upsertSummary(s); live.add(key(s.session_id, s.task_id)); }
    const fresh = [];
    for (const s of page?.tasks || []) {
      if (!live.has(key(s.session_id, s.task_id))) upsertSummary(s);
      fresh.push(key(s.session_id, s.task_id));
    }
    // 「全部」视图用新第一页重建（断档期间可能错过了分页位置）
    const vw = views.all;
    if (vw?.started) {
      vw.keys = [...new Set([...live, ...fresh, ...vw.keys.filter((k) => !fresh.includes(k))])];
      vw.cursor = page?.next_cursor ?? vw.cursor;
      vw.exhausted = vw.cursor == null;
    }
    for (const t of tasks.values()) {
      if (t.status !== 'running') continue;
      if (!live.has(t.key)) loadDetail(t);
      else if (t.detail === 'loaded') t.detail = 'stale'; // 断档期间的活动需重新回填
    }
    emitChange();
  } catch (e) {
    console.error('任务对账失败', e);
  }
}

/** 全局流事件 → 任务聚合。data 已解析，带 session_id。 */
function handleEvent(name, d) {
  const sid = d.session_id;
  if (!sid) return;
  if (name === 'task_started') {
    const t = ensureTask(sid, d.task_id, d.worker);
    if (!t.summaryKnown) { t.task = d.task || ''; t.startedAt = Date.now(); t.summaryKnown = true; }
    // 命中各已加载筛选的头部插入（all / running）
    for (const v of ['all', 'running']) {
      const vw = views[v];
      if (vw?.started && !vw.keys.includes(t.key)) vw.keys.unshift(t.key);
    }
    emitChange();
    return;
  }
  if (name === 'task_finished') {
    const t = tasks.get(key(sid, d.task_id)) || stub(sid, d.task_id, d.worker);
    t.status = d.error ? (d.error === 'cancelled' ? 'cancelled' : 'error') : 'done';
    t.finishedAt = t.finishedAt || Date.now();
    t.result = d.result ?? null;
    t.error = d.error ?? null;
    if (t.detail === 'loaded') t.detail = 'stale'; // 最后一段文本此时才落库
    t.dirty = true;
    emitChange();
    return;
  }
  const m = AGENT_RE.exec(d.agent || '');
  if (!m) return; // controller 事件：全局流不应出现
  const t = tasks.get(key(sid, Number(m[2]))) || stub(sid, Number(m[2]), m[1]);
  if (name === 'usage') {
    if (t.status === 'running') {
      t.liveTokens += d.usage?.total_tokens || 0;
      t.tokens = t.baseTokens + t.liveTokens;
    }
  } else {
    logLive(t, name, d);
  }
  emitChange();
}

function logLive(t, name, d) {
  const ts = Date.now();
  if (name === 'text_delta') {
    if (!d.text) return;
    const last = t.log[t.log.length - 1];
    // 同一 agent 连续的文本碎片并入同一段（与后端「按 agent 连续段合并存储」一致）
    if (last && last.kind === 'text' && last.agent === d.agent) last.text += d.text;
    else t.log.push({ kind: 'text', agent: d.agent, text: d.text, ts });
  } else if (name === 'tool_call') {
    t.log.push({ kind: 'call', agent: d.agent, id: d.id, name: d.name, arguments: d.arguments, ts });
  } else if (name === 'tool_result') {
    t.log.push({ kind: 'result', agent: d.agent, id: d.id, name: d.name, output: d.output, ts });
  }
  t.dirty = true;
}

// ---------------------------------------------------------------- 内部：活动流重放

function viewOf(t) {
  if (!t.dirty && t.view) return t.view;
  t.view = rebuild(t);
  t.dirty = false;
  return t.view;
}

/** persisted 全量 + log 去重后重放。去重：工具按 (agent,id)；文本按该 agent 已落库全文的子串。 */
function rebuild(t) {
  const v = { events: [], calls: new Map(), subs: new Map(), pendingSpawns: [] };
  const pCalls = new Set();
  const pResults = new Set();
  const pText = new Map(); // agent -> 已落库文本拼接
  for (const e of t.persisted) {
    if (e.kind === 'call') pCalls.add(callKey(e.agent, e.id));
    else if (e.kind === 'result') pResults.add(callKey(e.agent, e.id));
    else if (e.kind === 'text') pText.set(e.agent, (pText.get(e.agent) || '') + e.text);
    replay(v, e);
  }
  for (const e of t.log) {
    if (e.kind === 'call' && pCalls.has(callKey(e.agent, e.id))) continue;
    if (e.kind === 'result' && pResults.has(callKey(e.agent, e.id))) continue;
    if (e.kind === 'text' && (pText.get(e.agent) || '').includes(e.text)) continue;
    replay(v, e);
  }
  if (t.status !== 'running') {
    for (const call of v.calls.values()) if (call.status === 'running') call.status = 'done';
    for (const sub of v.subs.values()) sub.status = 'done';
  }
  return v;
}

function replay(v, e) {
  const box = container(v, e.agent, e.ts);
  if (e.kind === 'text') {
    const last = box[box.length - 1];
    if (last && last.kind === 'text' && last.agent === e.agent) last.text += e.text;
    else box.push({ kind: 'text', agent: e.agent, text: e.text, ts: e.ts });
  } else if (e.kind === 'call') {
    const call = {
      id: e.id, agent: e.agent, name: e.name, arguments: e.arguments, ts: e.ts,
      status: 'running', output: null, doneTs: null,
    };
    v.calls.set(callKey(e.agent, e.id), call);
    box.push({ kind: 'tool', call });
    if (e.name === 'spawn_subagent') {
      // 记下派生说明，第一个出现的 sub 事件认领
      try { v.pendingSpawns.push(JSON.parse(e.arguments).task || ''); } catch { v.pendingSpawns.push(''); }
    }
  } else if (e.kind === 'result') {
    const call = v.calls.get(callKey(e.agent, e.id));
    if (call) { call.output = e.output; call.status = 'done'; call.doneTs = e.ts; }
  }
}

/** agent 标签 → 事件容器：主 agent 用任务本身，子 agent（/subN）用子组（首见时创建）。 */
function container(v, agentLabel, ts) {
  const m = AGENT_RE.exec(agentLabel || '');
  if (!m || !m[3]) return v.events;
  const subName = m[3].slice(1); // "sub1"
  let sub = v.subs.get(subName);
  if (!sub) {
    sub = { name: subName, worker: m[1], note: v.pendingSpawns.shift() || '', events: [], status: 'running' };
    v.subs.set(subName, sub);
    v.events.push({ kind: 'sub', sub, ts });
  }
  return sub.events;
}

// ---------------------------------------------------------------- 内部：对话页逐会话 SSE

function sessionStream(id) {
  let s = streams.get(id);
  if (!s) {
    s = {
      id, conn: null, listeners: new Set(), state: 'idle',
      retryDelay: 1000, timer: null, openWaiters: [], controllerBusy: false,
    };
    streams.set(id, s);
  }
  connectSession(s);
  return s;
}

/** 指数退避 1s→30s；401 全局回登录；404 视为会话已删除。 */
function connectSession(s) {
  if (s.conn || s.timer) return;
  const auth = loadAuth();
  if (!auth) return;
  s.state = 'connecting';
  s.conn = connectSSE(`/api/sessions/${s.id}/events`, auth.token, {
    onOpen() {
      s.state = 'open';
      s.retryDelay = 1000;
      releaseWaiters(s);
    },
    onEvent(name, raw) {
      let data = raw;
      if (name !== 'lagged') {
        try { data = JSON.parse(raw); } catch { return; }
      }
      trackController(s, name, data);
      for (const fn of s.listeners) {
        try { fn(name, data); } catch (e) { console.error('会话事件监听者出错', e); }
      }
    },
    onClose(status) {
      s.conn = null;
      s.state = 'closed';
      releaseWaiters(s);
      if (streams.get(s.id) !== s) return;
      if (status === 401) { clearAuth(); unauthorized(); return; }
      if (status === 404) { TasksStore.unwatchSession(s.id); return; }
      s.timer = setTimeout(() => { s.timer = null; connectSession(s); }, s.retryDelay);
      s.retryDelay = Math.min(s.retryDelay * 2, 30000);
    },
  });
}

function releaseWaiters(s) {
  s.openWaiters.splice(0).forEach((fn) => fn());
}

function closeSessionStream(id) {
  const s = streams.get(id);
  if (!s) return;
  streams.delete(id);
  clearTimeout(s.timer);
  if (s.conn) s.conn.close();
  releaseWaiters(s);
  if (s.controllerBusy) emitChange();
}

/** 关闭没有订阅者、也没人在等 open 的会话流。 */
function pruneSessionStreams() {
  for (const s of [...streams.values()]) {
    if (!s.listeners.size && !s.openWaiters.length) closeSessionStream(s.id);
  }
}

/** controller 忙碌态（worker chip 用）：controller 的过程事件视为忙，reply/error 落闲。 */
function trackController(s, name, data) {
  let busy = s.controllerBusy;
  if (name === 'reply' || name === 'error') busy = false;
  else if ((name === 'text_delta' || name === 'tool_call' || name === 'usage') && !AGENT_RE.test(data?.agent || '')) busy = true;
  if (busy === s.controllerBusy) return;
  s.controllerBusy = busy;
  emitChange();
}

