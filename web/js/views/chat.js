// 会话视图：选择/加载会话、渲染历史、发送消息、SSE 连接与重连。

import { state } from '../state.js';
import { $, el, toast, reportError } from '../util.js';
import { api } from '../lib/api.js';
import { connectSSE } from '../lib/sse.js';
import {
  scrollBottom, clearMessages, resetRenderState, addUserBlock, replyBlock,
  ensureRun, ensureTaskCard, addToolCard, fillToolOutput, addNotice,
} from './run.js';
import { handleEvent } from './events.js';
import { renderSessionList, loadSessions, createSession } from './sessions.js';
import { logout } from './login.js';

const EMPTY_HINT = '选择左侧会话，或新建一个开始对话';

// ==================== 会话加载 ====================

/**
 * 拉取会话历史与运行中任务并渲染。每个 await 之后检查代次，
 * 用户已切到别的会话（或登出）就放弃，避免把 A 的内容渲染进 B。
 * @param {boolean} quiet  失败时保留现有内容、只弹提示（重连后的重拉用）
 * @returns {Promise<boolean>} 是否成功渲染（被取代或失败时返回 false）
 */
async function loadSessionContent(id, gen, quiet = false) {
  const stale = () => gen !== state.loadGen || state.currentId !== id;
  const enc = encodeURIComponent(id);
  try {
    const detail = await api(`/api/sessions/${enc}`);
    if (stale()) return false;
    // 恢复仍在运行的任务卡片（事件无回放，只能拿到清单）
    const tasks = await api(`/api/sessions/${enc}/tasks`);
    if (stale()) return false;

    resetRenderState();
    clearMessages();
    renderHistory(detail.history || []);
    if (tasks.length > 0) {
      ensureRun();
      for (const t of tasks) ensureTaskCard(String(t.task_id), t.worker, t.task);
    }
    if (!$('#messages').children.length) {
      clearMessages('这个会话还没有消息，从下方输入开始');
    }
    scrollBottom(true);
    return true;
  } catch (e) {
    if (stale()) return false;
    if (e.status === 404) {
      leaveSession('会话不存在或已被删除');
      loadSessions();
    } else if (quiet) {
      reportError('重新加载会话失败', e);
    } else {
      clearMessages(`加载会话失败：${e.message}`);
    }
    return false;
  }
}

export async function selectSession(id) {
  if (state.currentId === id && state.sse) return;
  const gen = ++state.loadGen;
  closeSSE();
  state.sseRetry = 0;
  state.sseReconnecting = false;
  state.resyncing = false;
  state.pendingEvents = [];
  state.currentId = id;
  resetRenderState();
  renderSessionList();

  const s = state.sessions.find((x) => x.id === id);
  $('#chat-title').textContent = s ? s.title || '新会话' : '会话';
  clearMessages('加载中…');

  if (await loadSessionContent(id, gen)) connectEvents();
}

/**
 * 重连成功后：后端不回放断线期间的事件，重新拉历史覆盖渲染。
 * 期间到达的实时事件先缓冲，渲染完再按序应用。
 */
async function resyncAfterReconnect() {
  const id = state.currentId;
  if (!id) return;
  const gen = ++state.loadGen;
  state.resyncing = true;
  state.pendingEvents = [];
  await loadSessionContent(id, gen, true);
  if (gen !== state.loadGen) return; // 已被新的选择/重载取代
  state.resyncing = false;
  const pending = state.pendingEvents;
  state.pendingEvents = [];
  // 无论重拉成功与否（失败时保留旧内容），都接着应用缓冲的实时事件
  for (const [name, data] of pending) handleEvent(name, data);
}

/** 离开当前会话：断开 SSE、清空消息区。 */
export function leaveSession(placeholder = EMPTY_HINT) {
  state.loadGen++;
  closeSSE();
  state.currentId = null;
  state.sseRetry = 0;
  state.sseReconnecting = false;
  state.resyncing = false;
  state.pendingEvents = [];
  resetRenderState();
  clearMessages(placeholder);
  $('#chat-title').textContent = '会话';
  setConn('off', '未连接');
  renderSessionList();
}

/** 登出 / 换用户时彻底重置会话视图。 */
export function resetChat() {
  leaveSession();
  $('#input').value = '';
  autogrow();
}

// ==================== 历史渲染 ====================

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
      if (m.content) runCtx.el.insertBefore(replyBlock(m.content), runCtx.toolsEl);
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

// ==================== 发送 ====================

export async function sendMessage() {
  const input = $('#input');
  const content = input.value.trim();
  if (!content) return;
  if (!state.currentId) {
    // 没有会话时自动建一个；失败已提示，保留输入内容
    if (!(await createSession())) return;
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

export function autogrow() {
  const input = $('#input');
  input.style.height = 'auto';
  input.style.height = Math.min(input.scrollHeight, 200) + 'px';
}

// ==================== SSE ====================

/** 连接状态：圆点颜色 + 文字（不只靠颜色区分）。 */
function setConn(status, text) {
  $('#conn-dot').className = `conn-dot ${status}`;
  $('#conn-text').textContent = text;
}

export function closeSSE() {
  if (state.sse) { state.sse.close(); state.sse = null; }
  if (state.sseTimer) { clearTimeout(state.sseTimer); state.sseTimer = null; }
}

function connectEvents() {
  closeSSE();
  const id = state.currentId;
  if (!id || !state.auth) return;
  setConn('retry', '连接中');
  state.sse = connectSSE(`/api/sessions/${encodeURIComponent(id)}/events`, state.auth.token, {
    onOpen: () => {
      state.sseRetry = 0;
      setConn('on', '已连接');
      if (state.sseReconnecting) {
        state.sseReconnecting = false;
        resyncAfterReconnect();
      }
    },
    onEvent: (name, data) => {
      if (state.currentId !== id) return;
      if (state.resyncing) { state.pendingEvents.push([name, data]); return; }
      handleEvent(name, data);
    },
    onClose: (status) => {
      state.sse = null;
      if (state.currentId !== id) return;
      if (status === 401) { logout(); return; }
      if (status >= 400 && status < 500) {
        // 4xx 不重连：404 = 会话已删除，其余（403 等）重试也不会成功
        if (status === 404) {
          leaveSession('会话不存在或已被删除');
          toast('当前会话已不存在');
          loadSessions();
        } else {
          setConn('off', `连接被拒绝（HTTP ${status}）`);
        }
        return;
      }
      setConn('retry', '重连中');
      const delay = Math.min(30000, 1000 * 2 ** state.sseRetry);
      state.sseRetry += 1;
      state.sseReconnecting = true;
      state.sseTimer = setTimeout(connectEvents, delay);
    },
  });
}
