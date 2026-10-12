// 「内心」视图（admin + 二次鉴权）：意识流 / 日记 / 自我 / 审计。
// POST /api/auth/elevate 换 inner_token（10 分钟），只放内存，不落存储；
// 之后的请求带 X-Inner-Token，403 视为过期，回到密码框。
// 每次查看都会写进审计，她的意识流里也会出现一条 observed —— 页面上如实告知。

import { api, apiRaw, ApiError, loadAuth } from './lib/api.js';
import { connectSSE } from './lib/sse.js';
import { renderMarkdown } from './lib/markdown.js';
import { h, fmtClock } from './lib/dom.js';

const PAGE = 100;
const TABS = [['stream', '意识流'], ['journal', '日记'], ['self', '自我'], ['audit', '审计']];
const DRIVE_LABEL = { missing: '想念', share: '分享', curiosity: '好奇', project: '计划', tidy: '整理' };
const FIELD_LABEL = { identity: '自述', values: '价值', interests: '兴趣', mood: '心情', projects: '在做的事' };

let onUnauthorized = () => {};
let isAdmin = false;
const names = new Map(); // person id → name（从 heard / observed 里学到；said.to 与审计只带 id）
const inner = { token: null, expiresAt: 0, tab: 'stream', live: null, oldest: null, exhausted: false, loading: false };

export const InnerView = {
  init({ onUnauthorized: fn }) {
    onUnauthorized = fn || onUnauthorized;
    document.querySelectorAll('#view-tabs button').forEach((b) => {
      b.addEventListener('click', () => show(b.dataset.view));
    });
    document.getElementById('inner-scroll').addEventListener('scroll', (e) => {
      const sc = e.currentTarget;
      if (inner.tab === 'stream' && sc.scrollTop + sc.clientHeight > sc.scrollHeight - 200) loadOlder();
    });
  },

  start(admin) {
    isAdmin = admin;
    lock();
    document.getElementById('view-tabs').hidden = !admin;
    show('talk');
  },
};

/** 切换 对话 / 内心。 */
function show(view) {
  const innerOn = view === 'inner' && isAdmin;
  document.querySelectorAll('#view-tabs button').forEach((b) => b.classList.toggle('on', b.dataset.view === (innerOn ? 'inner' : 'talk')));
  document.getElementById('home-scroll').hidden = innerOn;
  document.getElementById('home-footer').hidden = innerOn;
  document.getElementById('inner-scroll').hidden = !innerOn;
  if (!innerOn) { stopLive(); return; }
  if (!unlocked()) renderGate();
  else openTab(inner.tab);
}

function unlocked() {
  return !!inner.token && inner.expiresAt * 1000 > Date.now() + 5000;
}

function lock() {
  stopLive();
  inner.token = null;
  inner.expiresAt = 0;
}

// ---------------------------------------------------------------- 二次鉴权

function renderGate(message) {
  const col = column();
  const pass = h('input', { type: 'password', class: 'gate-input', autocomplete: 'current-password', 'aria-label': '管理员密码', placeholder: '管理员密码' });
  const err = h('div', { class: 'gate-err', role: 'alert', text: message || '' });
  const btn = h('button', { type: 'submit', class: 'gate-btn', text: '进入' });
  const form = h('form', { class: 'gate' },
    h('div', { class: 'gate-title', text: '她的内心' }),
    h('p', { class: 'gate-hint', text: '查看前需要再输入一次密码。每次查看都会记入审计，她也会知道有人看过。' }),
    // 密码管理器 / 无障碍：密码框所属的用户名
    h('input', { type: 'text', name: 'username', autocomplete: 'username', value: loadAuth()?.user?.username || '', hidden: '', 'aria-hidden': 'true', tabindex: '-1' }),
    pass, btn, err,
  );
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (!pass.value) return;
    btn.disabled = true;
    err.textContent = '';
    try {
      const r = await api('/api/auth/elevate', { method: 'POST', body: { password: pass.value } , skip401: true });
      inner.token = r.inner_token;
      inner.expiresAt = r.expires_at;
      openTab(inner.tab);
    } catch (ex) {
      if (ex instanceof ApiError && ex.status === 401) err.textContent = '密码不正确';
      else if (ex instanceof ApiError && ex.status === 429) err.textContent = '失败次数过多，请 15 分钟后再试';
      else err.textContent = ex.message;
    } finally {
      btn.disabled = false;
      pass.value = '';
    }
  });
  col.append(form);
  pass.focus();
}

/** 带 X-Inner-Token 的请求；403 = 过期或缺失，回到密码框。 */
async function innerApi(path) {
  if (!unlocked()) { lock(); renderGate('验证已过期，请重新输入密码'); throw new ApiError(403, 'expired'); }
  try {
    const resp = await apiRaw(path, { headers: { 'X-Inner-Token': inner.token } });
    const text = await resp.text();
    return text ? JSON.parse(text) : null;
  } catch (e) {
    if (e instanceof ApiError && e.status === 403) { lock(); renderGate('验证已过期，请重新输入密码'); }
    if (e instanceof ApiError && e.status === 401) onUnauthorized();
    throw e;
  }
}

// ---------------------------------------------------------------- 子页

function column() {
  const col = document.getElementById('inner-column');
  col.textContent = '';
  return col;
}

function openTab(tab) {
  inner.tab = tab;
  stopLive();
  const col = column();
  const left = Math.max(0, Math.round((inner.expiresAt * 1000 - Date.now()) / 60000));
  col.append(h('div', { class: 'inner-tabs', role: 'tablist' },
    ...TABS.map(([k, label]) => h('button', {
      type: 'button', role: 'tab', class: k === tab ? 'on' : null, 'aria-selected': String(k === tab),
      onclick: () => openTab(k), text: label,
    })),
    h('span', { class: 'inner-ttl', text: `验证剩余约 ${left} 分钟` }),
    h('button', { type: 'button', class: 'inner-lock', text: '锁定', onclick() { lock(); renderGate(); } }),
  ));
  const body = h('div', { class: 'inner-body', id: 'inner-body' });
  col.append(body);
  ({ stream: openStream, journal: openJournal, self: openSelf, audit: openAudit })[tab](body)
    .catch((e) => { if (!(e instanceof ApiError && (e.status === 403 || e.status === 401))) body.append(note(e.message)); });
}

function note(text) {
  return h('div', { class: 'inner-note', text });
}

// ---- 意识流：新的在上，往下滚加载更早的；实时流把新时刻插到顶上

async function openStream(body) {
  inner.oldest = null;
  inner.exhausted = false;
  body.append(h('div', { class: 'inner-live', id: 'inner-live' }), h('div', { class: 'stream', id: 'inner-stream' }));
  await loadOlder();
  startLive();
}

async function loadOlder() {
  const list = document.getElementById('inner-stream');
  if (!list || inner.loading || inner.exhausted) return;
  inner.loading = true;
  try {
    const q = inner.oldest == null ? '' : `&before=${inner.oldest}`;
    const page = await innerApi(`/api/mind/stream?limit=${PAGE}${q}`);
    for (const m of page) list.append(momentRow(m));
    if (page.length) inner.oldest = page[page.length - 1].seq;
    if (page.length < PAGE) {
      inner.exhausted = true;
      list.append(note(list.childElementCount ? '— 她的第一刻 —' : '意识流还是空的'));
    }
  } finally {
    inner.loading = false;
  }
}

function startLive() {
  const auth = loadAuth();
  if (!auth || !unlocked()) return;
  inner.live = connectSSE('/api/mind/stream/events', auth.token, {
    onOpen() { setLive('实时'); },
    onEvent(name, raw) {
      if (name === 'lagged') { setLive(`落后了 ${raw} 条，刷新可补齐`); return; }
      let ev; try { ev = JSON.parse(raw); } catch { return; }
      const list = document.getElementById('inner-stream');
      if (ev.type === 'moment' && list) {
        const row = momentRow(ev.moment);
        row.classList.add('fresh');
        list.prepend(row);
      } else if (ev.type === 'thinking') {
        setLive(ev.active ? '实时 · 她在想' : '实时');
      } else if (ev.type === 'turn_failed') {
        list?.prepend(h('div', { class: 'row failed' },
          h('span', { class: 't', text: fmtClock(Date.now()) }),
          h('span', { class: 'k', text: '失败' }),
          h('span', { class: 'x', text: ev.backoff_secs ? `${ev.error}（${ev.backoff_secs}s 后重试）` : ev.error }),
        ));
      }
    },
    onClose(status) {
      inner.live = null;
      if (status === 403) { lock(); renderGate('验证已过期，请重新输入密码'); return; }
      setLive('已断开');
    },
  }, { 'X-Inner-Token': inner.token });
}

function stopLive() {
  if (inner.live) { inner.live.close(); inner.live = null; }
}

function setLive(text) {
  const el = document.getElementById('inner-live');
  if (el) el.textContent = text;
}

/** 一行意识流：时间 · 类别 · 内容。 */
function momentRow(m) {
  const p = m.percept;
  const who = p?.from || p?.by;
  if (who?.id && who.name) names.set(who.id, who.name);
  const x = m.expressed;
  let kind = '';
  let text = '';
  let cls = m.dir || (p ? 'in' : 'out');
  if (p) {
    switch (p.kind) {
      case 'heard': kind = `${p.from?.name || p.from?.id} 说`; text = p.text; break;
      case 'task_done': kind = p.ok ? '任务完成' : '任务失败'; text = `${p.worker} #${p.task_id}：${p.result}`; break;
      case 'due': kind = '到期'; text = p.text; break;
      case 'drive': kind = DRIVE_LABEL[p.drive] || p.drive; text = `${p.reason}（${Number(p.pressure).toFixed(2)}）`; break;
      case 'observed': kind = '被看'; text = `${p.by?.name || p.by?.id} 看了${p.range}`; cls += ' muted'; break;
      case 'awake': kind = '醒来'; text = p.first ? '第一次醒来' : `睡了 ${Math.round((p.slept_ms || 0) / 60000)} 分钟`; break;
      default: kind = p.kind; text = JSON.stringify(p);
    }
  } else if (x) {
    switch (x.kind) {
      case 'said': kind = `对 ${nameOf(x.to)} 说`; text = x.text; break;
      case 'thought': kind = '心里'; text = x.text; cls += ' thought'; break;
      case 'acted': kind = x.tool; text = x.summary; break;
      default: kind = x.kind; text = JSON.stringify(x);
    }
  }
  return h('div', { class: `row ${cls}` },
    h('span', { class: 't', text: fmtStamp(m.at_ms), title: `#${m.seq}` }),
    h('span', { class: 'k', text: kind }),
    h('span', { class: 'x', text }),
  );
}

// ---- 日记 / 自我 / 审计

async function openJournal(body) {
  const items = await innerApi('/api/mind/journal?limit=100');
  if (!items.length) { body.append(note('她还没写过日记')); return; }
  for (const j of items) {
    body.append(h('section', { class: 'entry' },
      h('div', { class: 'entry-when', text: fmtStamp(j.at_ms) }),
      h('div', { class: 'body', html: renderMarkdown(j.text || '') }),
    ));
  }
}

async function openSelf(body) {
  const { self_model: s, history } = await innerApi('/api/mind/self');
  for (const [id, p] of Object.entries(s.people || {})) if (p.name) names.set(id, p.name);
  const kv = (k, v) => (v ? h('div', { class: 'kv' }, h('span', { class: 'k', text: k }), h('span', { class: 'v', text: v })) : null);
  body.append(h('section', { class: 'entry' },
    kv('自述', s.identity),
    kv('价值', (s.values || []).join('；')),
    kv('心情', s.mood ? `${s.mood.note || '—'}（愉快 ${fmt2(s.mood.valence)} · 激动 ${fmt2(s.mood.arousal)}）` : ''),
    kv('兴趣', (s.interests || []).map((i) => (i.why ? `${i.topic}（${i.why}）` : i.topic)).join('；')),
    kv('在做', (s.projects || []).map((p) => `${p.title}${p.status !== 'active' ? `［${p.status}］` : ''}${p.next ? ` → ${p.next}` : ''}`).join('；')),
    kv('认识的人', Object.entries(s.people || {}).map(([, p]) => (p.impression ? `${p.name}：${p.impression}` : p.name)).join('；')),
  ));
  body.append(h('div', { class: 'side-label', text: '成长轨迹' }));
  if (!history.length) { body.append(note('还没有变化')); return; }
  for (const c of history) {
    body.append(h('div', { class: 'row' },
      h('span', { class: 't', text: fmtStamp(c.at_ms) }),
      h('span', { class: 'k', text: FIELD_LABEL[c.field] || c.field }),
      h('span', { class: 'x', text: `${brief(c.before)} → ${brief(c.after)}${c.reason ? `  ·  ${c.reason}` : ''}` }),
    ));
  }
}

async function openAudit(body) {
  const items = await innerApi('/api/mind/audit');
  if (!items.length) { body.append(note('暂无审计记录')); return; }
  for (const a of items) {
    let kind = a.kind;
    let text = '';
    if (a.kind === 'inner_view') { kind = '查看'; text = `${nameOf(a.by)} 看了${a.what}`; }
    else if (a.kind === 'disclosure') { kind = '提及他人'; text = `对 ${nameOf(a.to)} 说话时想起了 ${(a.memory_ids || []).length} 条别人的私事：${a.text || ''}`; }
    else if (a.kind === 'core_changed') { kind = '改了核心'; text = (a.fields || []).map((f) => FIELD_LABEL[f] || f).join('、'); }
    else text = JSON.stringify(a);
    body.append(h('div', { class: `row${a.kind === 'inner_view' ? ' muted' : ''}` },
      h('span', { class: 't', text: fmtStamp(a.at_ms) }),
      h('span', { class: 'k', text: kind }),
      h('span', { class: 'x', text }),
    ));
  }
}

function nameOf(id) {
  if (!id) return '?';
  if (names.has(id)) return names.get(id);
  if (id === loadAuth()?.user?.id) return loadAuth().user.username;
  return id.length > 10 ? `${id.slice(0, 8)}…` : id;
}

function brief(v) {
  if (v == null) return '∅';
  const s = typeof v === 'string' ? v : JSON.stringify(v);
  return s.length > 80 ? `${s.slice(0, 80)}…` : s;
}

function fmt2(n) {
  return Number(n ?? 0).toFixed(2);
}

function fmtStamp(ms) {
  const d = new Date(ms);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  return sameDay ? fmtClock(ms) : `${d.getMonth() + 1}/${d.getDate()} ${fmtClock(ms)}`;
}
