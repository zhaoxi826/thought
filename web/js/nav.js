// 视图切换（会话 / 记忆 / 系统）。单独成模块以免视图之间循环依赖 main.js。

import { state } from './state.js';
import { $ } from './util.js';
import { loadMemoryView } from './views/memory.js';
import { loadSystemView } from './views/system.js';

export function switchView(view) {
  state.view = view;
  document.querySelectorAll('.side-tabs .tab').forEach((b) => {
    const active = b.dataset.view === view;
    b.classList.toggle('active', active);
    if (active) b.setAttribute('aria-current', 'page');
    else b.removeAttribute('aria-current');
  });
  for (const v of ['chat', 'memory', 'system']) {
    $(`#view-${v}`).classList.toggle('hidden', v !== view);
  }
  setSidebarOpen(false);
  if (view === 'memory') loadMemoryView();
  if (view === 'system') loadSystemView();
}

export function setSidebarOpen(open) {
  $('#app').classList.toggle('sidebar-open', open);
  $('#btn-sidebar-toggle').setAttribute('aria-expanded', String(open));
}
