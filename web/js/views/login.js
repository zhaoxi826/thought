// 登录 / 登出，以及切换用户时的状态重置。

import { state } from '../state.js';
import { $ } from '../util.js';
import { api, saveAuth, clearAuth } from '../lib/api.js';
import { loadSessions, renderSessionList } from './sessions.js';
import { resetChat } from './chat.js';
import { resetMemoryView } from './memory.js';
import { resetSystemView } from './system.js';
import { switchView } from '../nav.js';

/** 清掉上一个用户的全部数据与 DOM（登出、以及登录新用户前调用）。 */
function resetUserState() {
  state.epoch++; // 丢弃上一个用户仍在途的异步结果
  state.sessions = [];
  resetChat(); // 断开 SSE、currentId/toolCards/taskCards/runningTasks 清空、消息区清空
  renderSessionList();
  resetMemoryView();
  resetSystemView();
  switchView('chat');
}

export function showLogin() {
  resetUserState();
  state.auth = null;
  $('#app').classList.add('hidden');
  $('#login-view').classList.remove('hidden');
  $('#login-password').value = '';
  $('#login-error').classList.add('hidden');
  $('#login-username').focus();
}

export function showApp() {
  resetUserState(); // 换用户登录时不残留上一个用户的数据
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

export async function doLogin(e) {
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

export function logout() {
  clearAuth();
  showLogin();
}
