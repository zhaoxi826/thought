// 任务数据适配层（adapter）。
//
// 背景：后端任务注册表（GET /api/tasks 等）尚未上线。当前实现的数据源：
//   1. GET /api/sessions → 逐会话 GET /api/sessions/{id}/tasks 收集运行中任务；
//   2. 逐会话 SSE（GET /api/sessions/{id}/events）监听 task_started/task_finished/
//      tool_call/tool_result/text_delta/usage，按 task_id 与 agent 标签聚合。
// 因此任务历史只在页面存活期内有效（UI 已如实标注）。
//
// ★ 换数据源时（后端注册表上线后）只需改三处：
//   - loadInitial()      ：改为 GET /api/tasks（含历史与分页）；
//   - ensureStream(id)   ：改为订阅全局事件流 GET /api/tasks/events（一条连接）；
//   - 取消任务           ：POST /api/tasks/{task_id}/cancel 落地后放开 UI 按钮。
// UI（js/tasks.js）只依赖本模块的对外接口，不需要改动。
//
// 对外接口：
//   start(sessions)          启动：sessions = [{id,title}]
//   watchSession(s)          会话新建后纳入订阅
//   unwatchSession(id)       会话删除后退订并移除其任务
//   subscribe(id, fn)        追加一条会话事件监听（对话页复用同一条 SSE），返回退订函数
//   waitOpen(id)             等该会话的 SSE 进入 open（发消息前调用，避免丢 reply）
//   list()                   任务数组（running 在前，再按开始时间倒序）
//   get(key)                 单个任务
//   runningCount()           运行中任务数（rail 呼吸点用）
//   onChange(fn)             任何任务/计数变化时触发
//   agents / setAgents(a)    GET /api/agents 的配置缓存（worker chip 用）

import { api, loadAuth, clearAuth } from './lib/api.js';
import { connectSSE } from './lib/sse.js';

const AGENT_RE = /^([^#]+)#(\d+)(\/sub\d+)?$/; // "coder#3" / "coder#3/sub1"

const tasks = new Map();        // key `${sessionId}:${taskId}` -> task
const sessions = new Map();     // id -> { title, conn, listeners:Set, state, retryDelay, timer, openWaiters:[] }
const changeListeners = new Set();
let agents = null;              // { controller:{model}, workers:[...] }
let unauthorized = () => {};

export const TasksStore = {
  get agents() { return agents; },
  setAgents(a) { agents = a; emitChange(); },
  setUnauthorizedHandler(fn) { unauthorized = fn; },

  onChange(fn) { changeListeners.add(fn); return () => changeListeners.delete(fn); },

  list() {
    return [...tasks.values()].sort((a, b) => {
      if ((a.status === 'running') !== (b.status === 'running')) return a.status === 'running' ? -1 : 1;
      return (b.startedAt || 0) - (a.startedAt || 0);
    });
  },
  get(key) { return tasks.get(key); },
  runningCount() {
    let n = 0;
    for (const t of tasks.values()) if (t.status === 'running') n++;
    return n;
  },

  /** 启动：对已知会话逐一收集运行中任务并建立 SSE 订阅。 */
  async start(sessionList) {
    for (const s of sessionList) watchSession(s);
    // 运行中任务逐会话收集（该接口目前会顺带唤醒会话循环，见 backend-api.md）
    await Promise.allSettled(sessionList.map(async (s) => {
      try {
        const running = await api(`/api/sessions/${s.id}/tasks`);
        for (const t of running || []) {
          ensureTask(s.id, t.task_id, t.worker, t.task, /*restored*/ true);
        }
      } catch { /* 单会话失败不阻塞整体 */ }
    }));
    emitChange();
  },

  watchSession,
  unwatchSession(id) {
    const s = sessions.get(id);
    if (s) {
      clearTimeout(s.timer);
      if (s.conn) s.conn.close();
      sessions.delete(id);
    }
    for (const key of [...tasks.keys()]) if (key.startsWith(`${id}:`)) tasks.delete(key);
    emitChange();
  },

  /** 对话页复用同一条 SSE：追加监听者。返回退订函数。 */
  subscribe(id, fn) {
    const s = sessions.get(id);
    if (!s) return () => {};
    s.listeners.add(fn);
    return () => s.listeners.delete(fn);
  },

  /** 等 SSE 进入 open；已在 open 立即返回。失败/超时（8s）也返回，不阻塞发送。 */
  waitOpen(id) {
    const s = sessions.get(id);
    if (!s) return Promise.resolve();
    if (s.state === 'open') return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setTimeout(resolve, 8000);
      s.openWaiters.push(() => { clearTimeout(timer); resolve(); });
    });
  },
};

// ---------------------------------------------------------------- 内部

function emitChange() {
  for (const fn of changeListeners) {
    try { fn(); } catch (e) { console.error('TasksStore 监听者出错', e); }
  }
}

function key(sessionId, taskId) { return `${sessionId}:${taskId}`; }

function watchSession(s) {
  if (sessions.has(s.id)) return;
  sessions.set(s.id, {
    id: s.id, title: s.title,
    conn: null, listeners: new Set(),
    state: 'idle', retryDelay: 1000, timer: null, openWaiters: [],
  });
  ensureStream(s.id);
}

/** 建立（或重建）某会话的 SSE。指数退避 1s→30s；401 全局回登录；404 视为会话已删除。 */
function ensureStream(id) {
  const s = sessions.get(id);
  if (!s || s.conn) return;
  const auth = loadAuth();
  if (!auth) return;
  s.state = 'connecting';
  s.conn = connectSSE(`/api/sessions/${id}/events`, auth.token, {
    onOpen() {
      s.state = 'open';
      s.retryDelay = 1000;
      const waiters = s.openWaiters.splice(0);
      waiters.forEach((fn) => fn());
    },
    onEvent(name, raw) {
      let data = raw;
      if (name !== 'lagged') {
        try { data = JSON.parse(raw); } catch { return; }
      }
      handleEvent(id, name, data);
      for (const fn of s.listeners) {
        try { fn(name, data); } catch (e) { console.error('会话事件监听者出错', e); }
      }
    },
    onClose(status) {
      s.conn = null;
      if (!sessions.has(id)) return;
      if (status === 401) { clearAuth(); unauthorized(); return; }
      if (status === 404) { TasksStore.unwatchSession(id); return; }
      // 网络错误或服务端断开：指数退避重连（1s→30s）
      s.state = 'closed';
      const waiters = s.openWaiters.splice(0);
      waiters.forEach((fn) => fn());
      s.timer = setTimeout(() => { s.timer = null; ensureStream(id); }, s.retryDelay);
      s.retryDelay = Math.min(s.retryDelay * 2, 30000);
    },
  });
}

/** 会话事件 → 任务聚合。data 已是解析后的 JSON（lagged 除外）。 */
function handleEvent(sessionId, name, data) {
  switch (name) {
    case 'task_started': {
      ensureTask(sessionId, data.task_id, data.worker, data.task, false);
      emitChange();
      break;
    }
    case 'task_finished': {
      const t = tasks.get(key(sessionId, data.task_id));
      if (!t) break;
      t.finishedAt = Date.now();
      t.result = data.result ?? null;
      t.error = data.error ?? null;
      t.status = data.error ? (data.error === 'cancelled' ? 'cancelled' : 'error') : 'done';
      for (const call of t.calls.values()) if (call.status === 'running') call.status = 'done';
      emitChange();
      break;
    }
    case 'tool_call': {
      const loc = locate(sessionId, data.agent);
      if (!loc) break;
      const call = {
        id: data.id, agent: data.agent, name: data.name,
        arguments: data.arguments, ts: Date.now(),
        status: 'running', output: null, doneTs: null,
      };
      loc.task.calls.set(data.id, call);
      pushEvent(loc, { kind: 'tool', call });
      if (data.name === 'spawn_subagent') {
        // 记下派生说明，第一个出现的 sub 事件认领
        try { loc.task.pendingSpawns.push(JSON.parse(data.arguments).task || ''); } catch { loc.task.pendingSpawns.push(''); }
      }
      emitChange();
      break;
    }
    case 'tool_result': {
      const loc = locate(sessionId, data.agent);
      if (!loc) break;
      const call = loc.task.calls.get(data.id);
      if (call) {
        call.output = data.output;
        call.status = 'done';
        call.doneTs = Date.now();
      }
      emitChange();
      break;
    }
    case 'text_delta': {
      const loc = locate(sessionId, data.agent);
      if (!loc) { markController(sessionId, true); break; }
      const last = loc.task.events[loc.task.events.length - 1];
      // 同一 agent 连续的文本碎片并入同一条 AGENT 事件
      if (last && last.kind === 'text' && last.agent === data.agent) {
        last.text += data.text;
      } else {
        pushEvent(loc, { kind: 'text', agent: data.agent, text: data.text, ts: Date.now() });
      }
      emitChange();
      break;
    }
    case 'usage': {
      const loc = locate(sessionId, data.agent);
      if (!loc) { markController(sessionId, true); break; }
      loc.task.tokens += data.usage?.total_tokens || 0;
      emitChange();
      break;
    }
    case 'reply':
    case 'error': {
      markController(sessionId, false);
      break;
    }
    default: break; // reasoning_delta / lagged：任务页不呈现
  }
}

/** agent 标签 → 任务定位。controller 返回 null。 */
function locate(sessionId, agentLabel) {
  const m = AGENT_RE.exec(agentLabel || '');
  if (!m) return null;
  const t = tasks.get(key(sessionId, Number(m[2])));
  if (!t) return null;
  if (m[3]) {
    // 子 agent：定位/创建子组
    const subName = m[3].slice(1); // "sub1"
    let sub = t.subs.get(subName);
    if (!sub) {
      sub = { name: subName, worker: m[1], note: t.pendingSpawns.shift() || '', events: [], status: 'running' };
      t.subs.set(subName, sub);
      pushEvent({ task: t }, { kind: 'sub', sub, ts: Date.now() });
    }
    return { task: t, sub };
  }
  return { task: t, sub: null };
}

function pushEvent(loc, ev) {
  if (loc.sub) loc.sub.events.push(ev);
  else loc.task.events.push(ev);
}

function ensureTask(sessionId, taskId, worker, taskText, restored) {
  const k = key(sessionId, taskId);
  if (tasks.has(k)) return tasks.get(k);
  const t = {
    key: k, sessionId,
    sessionTitle: sessions.get(sessionId)?.title || '',
    taskId, worker,
    label: `${worker}#${taskId}`,
    task: taskText,
    status: 'running',
    // 恢复出来的运行中任务拿不到真实开始时间，以首次见到的时间起计
    startedAt: Date.now(), restored,
    finishedAt: null, result: null, error: null,
    tokens: 0,
    events: [], calls: new Map(), subs: new Map(), pendingSpawns: [],
  };
  tasks.set(k, t);
  return t;
}

/** controller 忙碌状态（worker chip 用）：任何 controller 事件视为忙，reply/error 落闲。 */
function markController(sessionId, busy) {
  const s = sessions.get(sessionId);
  if (!s || s.controllerBusy === busy) return;
  s.controllerBusy = busy;
  emitChange();
}

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

/** controller 是否忙碌（任一会话）。 */
export function controllerBusy() {
  for (const s of sessions.values()) if (s.controllerBusy) return true;
  return false;
}

