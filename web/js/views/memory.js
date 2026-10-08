// 记忆视图：最近 / 搜索 / 手动写入 / 删除。

import { state } from '../state.js';
import { $, el, button, relTime, reportError } from '../util.js';
import { api } from '../lib/api.js';

function showDisabled() {
  state.memoryEnabled = false;
  $('#memory-body').classList.add('hidden');
  $('#memory-disabled').classList.remove('hidden');
}

function listMessage(text) {
  const list = $('#memory-list');
  list.textContent = '';
  list.append(el('div', 'placeholder', text));
}

export async function loadMemoryView() {
  if (state.memoryLoaded) return;
  state.memoryLoaded = true;
  const epoch = state.epoch;
  try {
    const recent = await api('/api/memory?limit=20');
    if (epoch !== state.epoch) return;
    state.memoryEnabled = true;
    renderMemoryList(recent, false);
    switchMemoryTab('recent');
  } catch (e) {
    if (epoch !== state.epoch) return;
    if (e.status === 404) {
      showDisabled();
    } else {
      state.memoryLoaded = false; // 下次进入记忆视图重试
      if (e.status !== 401) listMessage(`加载失败：${e.message}`);
    }
  }
}

/** 登出 / 换用户：清空列表与输入，下次进入重新加载。 */
export function resetMemoryView() {
  state.memoryLoaded = false;
  state.memoryEnabled = true;
  state.memoryTab = 'search';
  $('#memory-list').textContent = '';
  $('#memory-q').value = '';
  $('#memory-content').value = '';
  $('#memory-tags').value = '';
  $('#memory-write-msg').classList.add('hidden');
  $('#memory-body').classList.remove('hidden');
  $('#memory-disabled').classList.add('hidden');
  setTabButtons('search');
  $('#memory-search-row').classList.remove('hidden');
}

function setTabButtons(tab) {
  document.querySelectorAll('#view-memory .memory-tabs .tab').forEach((b) => {
    const active = b.dataset.mtab === tab;
    b.classList.toggle('active', active);
    b.setAttribute('aria-pressed', String(active));
  });
}

export function switchMemoryTab(tab) {
  state.memoryTab = tab;
  setTabButtons(tab);
  $('#memory-search-row').classList.toggle('hidden', tab !== 'search');
  if (tab === 'recent') loadRecentMemory();
}

async function loadRecentMemory() {
  const epoch = state.epoch;
  try {
    const items = await api('/api/memory?limit=50');
    if (epoch === state.epoch) renderMemoryList(items, false);
  } catch (e) {
    if (epoch !== state.epoch) return;
    if (e.status === 404) showDisabled();
    else reportError('加载记忆失败', e);
  }
}

export async function searchMemory() {
  const q = $('#memory-q').value.trim();
  if (!q) return;
  const epoch = state.epoch;
  try {
    const items = await api(`/api/memory/search?q=${encodeURIComponent(q)}&limit=20`);
    if (epoch === state.epoch) renderMemoryList(items, true);
  } catch (e) {
    if (epoch === state.epoch && e.status !== 401) listMessage(`搜索失败：${e.message}`);
  }
}

function renderMemoryList(items, withScore) {
  const list = $('#memory-list');
  list.textContent = '';
  if (!items.length) {
    list.append(el('div', 'placeholder', withScore ? '没有匹配的记忆' : '暂无记忆'));
    return;
  }
  for (const m of items) {
    const item = el('div', 'memory-item');
    item.append(el('div', 'memory-content', m.content));

    if (withScore && m.relations && m.relations.length) {
      item.append(el('div', 'memory-relations', m.relations.join('  ·  ')));
    }

    const meta = el('div', 'memory-meta');
    if (withScore && typeof m.score === 'number') {
      const bar = el('span', 'score-bar');
      bar.setAttribute('aria-hidden', 'true');
      const fill = el('i');
      fill.style.width = `${Math.round(Math.min(1, m.score) * 100)}%`;
      bar.append(fill);
      meta.append(bar, el('span', 'score-num', m.score.toFixed(3)));
      for (const ch of ['semantic', 'sparse', 'graph']) {
        const v = m.channels && m.channels[ch];
        if (typeof v === 'number') {
          const badge = el('span', 'channel-badge');
          badge.append(`${ch} `, Object.assign(el('b'), { textContent: v.toFixed(2) }));
          meta.append(badge);
        }
      }
    }
    for (const t of m.tags || []) meta.append(el('span', 'tag-chip', t));
    meta.append(el('span', null, relTime(m.created_at_ms)));
    const del = button('memory-del', '删除', '删除这条记忆');
    del.addEventListener('click', () => deleteMemory(m.id, item));
    meta.append(del);
    item.append(meta);
    list.append(item);
  }
}

async function deleteMemory(id, item) {
  if (!confirm('删除这条记忆？')) return;
  try {
    await api(`/api/memory/${encodeURIComponent(id)}`, { method: 'DELETE' });
    item.remove();
  } catch (e) {
    reportError('删除记忆失败', e);
  }
}

export async function writeMemory(e) {
  e.preventDefault();
  const msg = $('#memory-write-msg');
  const content = $('#memory-content').value.trim();
  if (!content) return;
  const tags = $('#memory-tags').value.split(',').map((t) => t.trim()).filter(Boolean);
  try {
    await api('/api/memory', { method: 'POST', body: { content, tags } });
    msg.textContent = '已写入';
    msg.className = 'form-msg';
    $('#memory-content').value = '';
    if (state.memoryTab === 'recent') loadRecentMemory();
  } catch (e2) {
    msg.textContent = `写入失败：${e2.message}`;
    msg.className = 'form-msg err';
  }
  msg.classList.remove('hidden');
}
