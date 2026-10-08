// thought 前端入口：绑定全局事件、恢复登录态。

import { state } from './state.js';
import { $ } from './util.js';
import { api, loadAuth, saveAuth, setUnauthorizedHandler } from './lib/api.js';
import { switchView, setSidebarOpen } from './nav.js';
import { doLogin, logout, showLogin, showApp } from './views/login.js';
import { createSession } from './views/sessions.js';
import { sendMessage, autogrow } from './views/chat.js';
import { writeMemory, searchMemory, switchMemoryTab } from './views/memory.js';

function bindEvents() {
  $('#login-form').addEventListener('submit', doLogin);
  $('#btn-logout').addEventListener('click', logout);
  $('#btn-new-session').addEventListener('click', createSession); // 内部已捕获错误并提示
  $('#btn-send').addEventListener('click', sendMessage);
  $('#btn-sidebar-toggle').addEventListener('click', () => {
    setSidebarOpen(!$('#app').classList.contains('sidebar-open'));
  });

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
