// 设置页：GET /api/config 填充全部分区；admin 可编辑，「保存」→ PUT /api/config（If-Match）。
//
// 模式：
//   edit    admin 且服务从配置文件启动：全部分区可编辑（草稿 = GET 响应的深拷贝，原样 PUT 回去；
//           响应里省略的密钥字段由服务端保留）；
//   nofile  GET /api/config → 404：退回 GET /api/models + GET /api/agents 只读呈现；
//   viewer  非 admin（403）：整页「仅管理员可见」空态；
//   error   其他失败：提示错误。
// 「原始 TOML」视图：GET /api/config?format=toml，只读（反映已保存的文件，不含未保存修改）。
// 密钥从不经过前端：*_env 字段只是环境变量名，按普通文本编辑。
// 表单部件在 js/settings-form.js。

import { api, apiRaw, ApiError } from './lib/api.js';
import { h } from './lib/dom.js';
import { TasksStore } from './tasks-store.js';
import { wireInput, wireSwitch, renderModels, renderAgent, resetForm } from './settings-form.js';

let mode = 'viewer';
let loadError = '';
let etag = null;
let draft = null;           // 编辑中的配置（edit 模式）
let dirty = false;
let saving = false;
let view = 'form';
let tomlSeq = 0;
let toastTimer = null;
const invalid = new Set();  // 当前格式不合法的输入框

// PUT 响应里的节名 → 界面用语
const SECTION_NAME = {
  http: '服务', storage: '存储', auth: '认证', embedding: '向量化', sandbox: '沙盒',
  mind: '心智', memory: '记忆', models: '模型池',
  'agent.controller': '主控', 'agent.workers': 'worker', 'agent.limits': '限额', budget: '预算',
};
const secName = (s) => SECTION_NAME[s] || s;

export const SettingsPage = {
  init() {
    document.getElementById('tab-form').addEventListener('click', () => switchView('form'));
    document.getElementById('tab-toml').addEventListener('click', () => switchView('toml'));
    document.getElementById('save-btn').addEventListener('click', save);
    // 可折叠子区（检索参数 / 出身 / 心跳 / 驱动）
    document.getElementById('form-view').addEventListener('click', (e) => {
      const head = e.target.closest('.subsec-head');
      if (head) head.parentElement.classList.toggle('open');
    });
    window.addEventListener('beforeunload', (e) => {
      if (dirty) { e.preventDefault(); e.returnValue = ''; }
    });
  },

  async start() {
    await load();
  },
};

// ---------------------------------------------------------------- 加载

async function load({ keepNotice = false } = {}) {
  try {
    const resp = await apiRaw('/api/config');
    etag = resp.headers.get('ETag');
    draft = await resp.json();
    mode = 'edit';
  } catch (e) {
    draft = null;
    etag = null;
    if (e instanceof ApiError && e.status === 403) mode = 'viewer';
    else if (e instanceof ApiError && e.status === 404) mode = 'nofile';
    else { mode = 'error'; loadError = e.message; }
  }
  dirty = false;
  invalid.clear();
  resetForm();
  if (mode === 'edit') renderEdit();
  else await renderFallback();
  paintMode();
  paintNotes(keepNotice);
  paintSave();
  if (view === 'toml') loadToml();
}

/** viewer：整页空态；其余模式正常显示表单。 */
function paintMode() {
  const form = document.getElementById('form-view');
  let empty = document.getElementById('settings-empty');
  if (mode === 'viewer') {
    form.style.display = 'none';
    if (!empty) {
      empty = h('div', { class: 'page-empty', id: 'settings-empty', text: '仅管理员可见' });
      form.after(empty);
    }
    empty.style.display = '';
  } else {
    form.style.display = '';
    if (empty) empty.style.display = 'none';
  }
}

function ctxOf(editable) {
  return { editable, onChange: markDirty, invalid };
}

function renderEdit() {
  const ctx = ctxOf(true);
  bindStatic(draft, ctx);
  renderModels(document.getElementById('models-body'), draft.models || (draft.models = {}), ctx);
  renderAgent(document.getElementById('agent-body'), draft.agent || {}, ctx);
}

async function renderFallback() {
  if (mode === 'viewer') return; // 整页空态，不填任何字段
  const ctx = ctxOf(false);
  const modelsBox = document.getElementById('models-body');
  const agentBox = document.getElementById('agent-body');
  const [models, agents] = await Promise.allSettled([
    mode === 'nofile' ? api('/api/models') : Promise.reject(new ApiError(403, 'admin only')),
    api('/api/agents'),
  ]);
  // 只读呈现：limits 取自 GET /api/agents；其余分区无数据源，保持占位
  bindStatic({ agent: { limits: agents.status === 'fulfilled' ? agents.value.limits : null } }, ctx);

  if (models.status === 'fulfilled') renderModels(modelsBox, models.value || {}, ctx);
  else cardNote(modelsBox, models.reason?.status === 403 ? '仅管理员可见' : `加载失败：${models.reason?.message}`);

  if (agents.status === 'fulfilled') renderAgent(agentBox, agents.value || {}, ctx);
  else cardNote(agentBox, `加载失败：${agents.reason?.message}`);
}

function cardNote(box, text) {
  box.textContent = '';
  box.append(h('div', { class: 'card-note', text }));
}

/** index.html 骨架里的静态字段：data-cfg（输入）/ data-cfg-area（多行文本）/
 *  data-cfg-lines（字符串数组，一行一个）/ data-cfg-switch（开关）。 */
function bindStatic(cfg, ctx) {
  const root = document.getElementById('form-view');
  root.querySelectorAll('[data-cfg]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfg);
    wireInput(el, ctx, obj, prop, { type: el.dataset.type || 'str', nullable: 'nullable' in el.dataset });
  });
  root.querySelectorAll('[data-cfg-area]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfgArea);
    const editable = ctx.editable && obj != null;
    el.value = obj?.[prop] ?? '';
    el.disabled = !editable;
    el.oninput = editable ? () => { obj[prop] = el.value; ctx.onChange(); } : null;
  });
  root.querySelectorAll('[data-cfg-lines]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfgLines);
    const editable = ctx.editable && obj != null;
    el.value = (obj?.[prop] || []).join('\n');
    el.disabled = !editable;
    el.oninput = editable ? () => {
      obj[prop] = el.value.split('\n').map((s) => s.trim()).filter(Boolean);
      ctx.onChange();
    } : null;
  });
  root.querySelectorAll('[data-cfg-switch]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfgSwitch);
    wireSwitch(el, ctx, obj, prop);
  });
}

/** "a.b.c" → [cfg.a.b, "c"]；中间节为 null（如 memory 未启用）时 obj 为 null。 */
function resolve(cfg, path) {
  const parts = path.split('.');
  const prop = parts.pop();
  let obj = cfg;
  for (const p of parts) obj = obj == null ? null : obj[p];
  return [obj ?? null, prop];
}

// ---------------------------------------------------------------- 卡片标注 / 提示 / 保存按钮

/** 卡片头右侧小字。edit 模式下不加标注（生效方式由保存结果给出）。 */
function noteFor(card) {
  if (mode === 'edit') {
    if (card === 'memory' && draft?.memory == null) return '未启用';
    if (card === 'mind' && draft?.mind == null) return '未启用';
    return '';
  }
  if (mode === 'nofile') {
    if (card === 'models' || card === 'agent' || card === 'limits') return '只读 · 服务未从配置文件启动';
    return '服务未从配置文件启动';
  }
  return '';
}

function paintNotes(keepNotice) {
  document.querySelectorAll('#form-view [data-note]').forEach((el) => { el.textContent = noteFor(el.dataset.note); });
  if (keepNotice) return;
  if (mode === 'error') notice(`配置加载失败：${loadError}`, 'err');
  else if (mode === 'nofile') notice('服务未从配置文件启动，配置不可读写');
  else notice('');
}

function notice(text, kind) {
  const el = document.getElementById('settings-note');
  el.textContent = text;
  el.classList.toggle('is-err', kind === 'err');
  el.classList.toggle('is-ok', kind === 'ok');
}

function markDirty() {
  if (!dirty) notice('有未保存的修改');
  dirty = true;
  paintSave();
}

function paintSave() {
  const btn = document.getElementById('save-btn');
  const can = mode === 'edit' && dirty && !saving;
  btn.disabled = !can;
  btn.textContent = saving ? '保存中…' : '保存';
  btn.title = mode === 'edit'
    ? (dirty ? '' : '没有未保存的修改')
    : mode === 'viewer' ? '仅管理员可修改配置' : '当前不可写回配置';
}

// ---------------------------------------------------------------- toast（保存结果 / 错误）

function toast(build, isErr = false) {
  let el = document.getElementById('settings-toast');
  if (!el) {
    el = h('div', { class: 'toast', id: 'settings-toast', role: 'status', 'aria-live': 'polite' });
    document.querySelector('.page-settings').append(el);
  }
  el.textContent = '';
  build(el);
  el.classList.toggle('is-err', isErr);
  el.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('show'), 6000);
}

function toastError(message) {
  toast((el) => { el.textContent = message; }, true);
}

/** 保存成功：已生效（绿 chip）/ 需重启（朱红 chip）。 */
function toastSaved(res) {
  const restart = (res?.restart_required || []).map(secName);
  const hot = (res?.hot_applied || []).map(secName);
  toast((el) => {
    el.append(h('div', { class: 't-row' }, '已保存'));
    if (hot.length) {
      el.append(h('div', { class: 't-row' },
        h('span', { class: 't-label', text: '已生效' }),
        ...hot.map((s) => h('span', { class: 't-chip ok', text: s })),
      ));
    }
    if (restart.length) {
      el.append(h('div', { class: 't-row' },
        h('span', { class: 't-label', text: '需重启' }),
        ...restart.map((s) => h('span', { class: 't-chip restart', text: s })),
      ));
    }
  });
}

// ---------------------------------------------------------------- 保存

async function save() {
  if (mode !== 'edit' || !dirty || saving) return;
  for (const el of [...invalid]) if (!el.isConnected) invalid.delete(el);
  if (invalid.size) {
    const first = invalid.values().next().value;
    first.focus();
    notice(`「${first.getAttribute('aria-label') || '字段'}」不是合法的数值`, 'err');
    return;
  }
  saving = true;
  paintSave();
  try {
    const res = await api('/api/config', {
      method: 'PUT',
      body: draft,
      headers: etag ? { 'If-Match': etag } : {},
    });
    saving = false;
    await load({ keepNotice: true });
    notice('');
    toastSaved(res);
    refreshAgents();
  } catch (e) {
    saving = false;
    paintSave();
    if (e instanceof ApiError && e.status === 412) {
      // 配置已在别处被修改：提示后重新拉取
      toastError('配置已被修改，已重新加载');
      notice('');
      await load({ keepNotice: true });
    } else if (e instanceof ApiError && e.status === 422) {
      notice(`校验未通过：${e.message}`, 'err');
    } else {
      notice(`保存失败：${e.message}`, 'err');
    }
  }
}

/** 保存后刷新任务页用到的 agent 配置缓存（worker chip / 并发上限）。 */
async function refreshAgents() {
  try { TasksStore.setAgents(await api('/api/agents')); } catch { /* 忽略 */ }
}

// ---------------------------------------------------------------- 表单 / 原始 TOML 切换

function switchView(v) {
  view = v;
  const toToml = v === 'toml';
  document.getElementById('form-view').style.display = toToml ? 'none' : '';
  const empty = document.getElementById('settings-empty');
  if (empty) empty.style.display = toToml ? 'none' : empty.style.display;
  document.getElementById('toml-view').style.display = toToml ? 'block' : 'none';
  document.getElementById('tab-form').classList.toggle('active', !toToml);
  document.getElementById('tab-toml').classList.toggle('active', toToml);
  if (toToml) loadToml();
  else paintMode();
}

async function loadToml() {
  const pre = document.getElementById('toml-pre');
  const seq = ++tomlSeq;
  pre.textContent = '加载中……';
  let text;
  try {
    const resp = await apiRaw('/api/config?format=toml');
    text = await resp.text();
  } catch (e) {
    if (seq !== tomlSeq) return;
    if (e instanceof ApiError && e.status === 403) pre.textContent = '仅管理员可见';
    else if (e instanceof ApiError && e.status === 404) pre.textContent = '服务未从配置文件启动';
    else pre.textContent = `加载失败：${e.message}`;
    return;
  }
  if (seq !== tomlSeq) return;
  paintToml(pre, text);
}

/** 轻量高亮：注释 / [节] / 键名。只读。 */
function paintToml(pre, text) {
  pre.textContent = '';
  for (const line of text.split('\n')) {
    const trimmed = line.trimStart();
    if (trimmed.startsWith('#')) {
      pre.append(h('span', { class: 'cm', text: line }));
    } else if (/^\[\[?[^\]]+\]\]?\s*(#.*)?$/.test(trimmed)) {
      pre.append(h('span', { class: 'sec', text: line }));
    } else {
      const m = /^(\s*)([A-Za-z0-9_.\-"]+)(\s*=.*)$/.exec(line);
      if (m) pre.append(m[1], h('span', { class: 'k', text: m[2] }), m[3]);
      else pre.append(line);
    }
    pre.append('\n');
  }
}
