// 侧栏底部「记忆」悬浮面板：最近记忆 + 搜索（GET /api/memory、/api/memory/search）。

import { api, ApiError } from './lib/api.js';
import { h, fmtRel } from './lib/dom.js';

let enabled = null; // null=未知 true/false

export const MemoryPanel = {
  init() {
    const entry = document.getElementById('memory-entry');
    const pop = document.getElementById('memory-pop');
    const search = document.getElementById('memory-search');

    // 点击固定/取消固定（hover 展开由 CSS 负责）；面板内部点击不冒泡
    pop.addEventListener('click', (e) => e.stopPropagation());
    entry.addEventListener('click', () => entry.classList.toggle('pin'));
    entry.addEventListener('mouseenter', () => { if (enabled === null) loadRecent(); });

    let debounce = null;
    search.addEventListener('input', () => {
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        const q = search.value.trim();
        if (q) searchMemory(q); else loadRecent();
      }, 300);
    });
  },
};

async function loadRecent() {
  const list = document.getElementById('memory-list');
  const count = document.getElementById('memory-count');
  document.getElementById('memory-pop-title').textContent = '最近记忆';
  try {
    const mems = await api('/api/memory?limit=8');
    enabled = true;
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
    list.textContent = '';
    if (e instanceof ApiError && e.status === 404) {
      enabled = false;
      count.textContent = '未启用';
      list.append(h('div', { class: 'mem-empty', text: '记忆未启用' }));
    } else {
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
  document.getElementById('memory-pop-title').textContent = '搜索';
  try {
    const hits = await api(`/api/memory/search?q=${encodeURIComponent(q)}&limit=8`);
    enabled = true;
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
      enabled = false;
      list.append(h('div', { class: 'mem-empty', text: '记忆未启用' }));
    } else {
      list.append(h('div', { class: 'mem-empty', text: e.message }));
    }
  }
}
