// 侧栏「她」：GET /api/mind/status —— 心情、自述、兴趣、在做的事、驱动压力、疲劳。
// 驱动的 reason 属于她的内心：只给 admin 以悬停提示显示，普通用户只看到压力条。
// 每 30s（页面可见时）与她每次想完后刷新。

import { api, ApiError, loadAuth } from './lib/api.js';
import { h } from './lib/dom.js';

const POLL_MS = 30_000;
const DRIVE_LABEL = { missing: '想念', share: '分享', curiosity: '好奇', project: '计划', tidy: '整理' };

let focusTask = () => {};
let timer = null;
let last = null;
let thinking = false;
let disabled = false;

export const HerPanel = {
  init({ focusTask: ft }) {
    focusTask = ft || focusTask;
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'visible' && !disabled) refresh();
    });
  },

  start() {
    last = null;
    disabled = false;
    render();
    refresh();
  },

  /** 她开始/结束想：想完后状态（心情、在做的事）可能变了，刷新一次。 */
  setThinking(active) {
    const was = thinking;
    thinking = active;
    if (was && !active) refresh();
    else render();
  },
};

async function refresh() {
  clearTimeout(timer);
  try {
    last = await api('/api/mind/status');
    thinking = !!last.thinking;
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) { disabled = true; last = null; render(); return; }
    if (e instanceof ApiError && e.status === 401) return;
  }
  render();
  timer = setTimeout(() => { if (document.visibilityState === 'visible') refresh(); else timer = null; }, POLL_MS);
}

function render() {
  const box = document.getElementById('her');
  box.textContent = '';
  if (disabled) {
    box.append(h('div', { class: 'her-empty', text: '心智未启用' }));
    return;
  }
  if (!last) {
    box.append(h('div', { class: 'her-empty', text: '…' }));
    return;
  }
  const admin = loadAuth()?.user?.role === 'admin';

  // 心情：点的颜色随愉快程度，呼吸随激动程度；note 是她自己写的
  const mood = last.mood || {};
  const dot = h('span', { class: `mood-dot ${moodTone(mood.valence)}${thinking ? ' live' : ''}` });
  dot.style.opacity = String(0.55 + 0.45 * clamp01(mood.arousal ?? 0.4));
  box.append(h('div', { class: 'her-mood' },
    dot,
    h('span', { class: 'note', text: thinking ? '在想事情' : (mood.note || '安静') }),
  ));

  if (last.identity) box.append(h('p', { class: 'her-identity', text: last.identity, title: last.identity }));

  section(box, '兴趣', last.interests?.length
    ? h('div', { class: 'her-chips' }, ...last.interests.slice(0, 8).map((t) => h('span', { class: 'chip', text: t })))
    : null);

  const doing = [
    ...(last.projects || []).map((p) => h('div', { class: 'her-item', text: p })),
    ...(last.tasks || []).map((t) => h('a', {
      class: 'her-item task', title: t.task, role: 'link', tabindex: '0',
      onclick: () => focusTask(`mind:${t.task_id}`),
      onkeydown: (e) => { if (e.key === 'Enter') focusTask(`mind:${t.task_id}`); },
    }, h('span', { class: 'w', text: t.worker }), document.createTextNode(t.task))),
  ];
  section(box, '在做', doing.length ? h('div', {}, ...doing) : null);

  section(box, '心', drives(last.heart, last.budget, admin));
}

function section(box, label, body) {
  if (!body) return;
  box.append(h('div', { class: 'side-label', text: label }), body);
}

/** 驱动压力条：刻度线是点火阈值；疲劳抬升阈值，单独一条。 */
function drives(heart, budget, admin) {
  if (!heart?.readings?.length) return null;
  const threshold = clamp01(heart.threshold ?? 0.6);
  const rows = heart.readings.map((r) => {
    const p = clamp01(r.pressure);
    const fill = h('i', {});
    fill.style.width = `${Math.round(p * 100)}%`;
    const tick = h('b', {});
    tick.style.left = `${Math.round(threshold * 100)}%`;
    return h('div', {
      class: `drive${p >= threshold ? ' hot' : ''}${heart.last_fired === r.name ? ' fired' : ''}`,
      title: admin && r.reason ? r.reason : null,
    },
      h('span', { class: 'n', text: DRIVE_LABEL[r.name] || r.name }),
      h('span', { class: 'bar' }, fill, tick),
    );
  });
  if (budget) {
    const f = Math.max(0, budget.fatigue || 0);
    const fill = h('i', {});
    fill.style.width = `${Math.round(Math.min(f, 1) * 100)}%`;
    rows.push(h('div', { class: 'drive fatigue', title: `今日自主用量 ${Math.round(f * 100)}%（软额度）` },
      h('span', { class: 'n', text: '疲劳' }),
      h('span', { class: 'bar' }, fill),
    ));
    if (budget.paused || budget.over_hard_limit) {
      rows.push(h('div', { class: 'her-paused', text: budget.paused ? '自主活动已暂停' : '今日额度已用完，她在休息' }));
    }
  }
  return h('div', { class: 'drives' }, ...rows);
}

function moodTone(v) {
  if (v == null) return 'calm';
  if (v > 0.25) return 'warm';
  if (v < -0.25) return 'low';
  return 'calm';
}

function clamp01(x) {
  const n = Number(x);
  return Number.isFinite(n) ? Math.max(0, Math.min(1, n)) : 0;
}
