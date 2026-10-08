// 侧栏会话列表：加载、渲染、新建、删除。

import { state } from '../state.js';
import { $, el, button, relTime, reportError } from '../util.js';
import { api } from '../lib/api.js';
import { selectSession, leaveSession } from './chat.js';
import { switchView } from '../nav.js';

export async function loadSessions() {
  const epoch = state.epoch;
  try {
    const sessions = await api('/api/sessions');
    if (epoch !== state.epoch) return; // 期间已登出/换用户
    state.sessions = sessions;
    renderSessionList();
  } catch (e) {
    if (epoch === state.epoch) reportError('加载会话列表失败', e);
  }
}

export function renderSessionList() {
  const list = $('#session-list');
  list.textContent = '';
  for (const s of state.sessions) {
    const active = s.id === state.currentId;
    const title = s.title || '新会话';
    const item = el('div', 'session-item' + (active ? ' active' : ''));

    // 主体是真正的按钮：可 Tab 聚焦，Enter/Space 激活
    const open = button('session-open');
    if (active) open.setAttribute('aria-current', 'true');
    open.append(el('span', 'session-title', title));
    open.append(el('span', 'session-time', relTime(s.updated_at_ms)));
    open.addEventListener('click', () => selectSession(s.id));

    const del = button('session-del', '×', `删除会话「${title}」`);
    del.title = '删除会话';
    del.addEventListener('click', () => deleteSession(s));

    item.append(open, del);
    list.append(item);
  }
}

/** 新建会话并切过去。成功返回会话 id，失败已提示并返回 null。 */
export async function createSession() {
  let s;
  try {
    s = await api('/api/sessions', { method: 'POST' });
  } catch (e) {
    reportError('新建会话失败', e);
    return null;
  }
  await loadSessions();
  await selectSession(s.id);
  switchView('chat');
  $('#input').focus();
  return s.id;
}

async function deleteSession(s) {
  if (!confirm(`删除会话「${s.title || '新会话'}」？此操作不可恢复。`)) return;
  try {
    await api(`/api/sessions/${encodeURIComponent(s.id)}`, { method: 'DELETE' });
  } catch (e) {
    reportError('删除会话失败', e);
    return;
  }
  if (state.currentId === s.id) leaveSession();
  await loadSessions();
}
