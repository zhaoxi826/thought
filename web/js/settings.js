// 设置页：GET /api/config 填充全部分区；admin 可编辑，「保存」→ PUT /api/config（If-Match）。
//
// 模式：
//   edit    admin 且服务从配置文件启动：全部分区可编辑（草稿 = GET 响应的深拷贝，原样 PUT 回去；
//           响应里省略的密钥字段由服务端保留）；
//   nofile  GET /api/config → 404：退回 GET /api/models + GET /api/agents 只读呈现；
//   viewer  非 admin（403）：模型池「仅管理员可见」，Agent / 限额来自 GET /api/agents 只读；
//   error   其他失败：同 viewer 呈现并提示错误。
// 「原始 TOML」视图：GET /api/config?format=toml，只读（反映已保存的文件，不含未保存修改）。
// 密钥从不经过前端：只显示 *_env 变量名。表单部件在 js/settings-form.js。

import { api, apiRaw, ApiError } from './lib/api.js';
import { h } from './lib/dom.js';
import { TasksStore } from './tasks-store.js';
import { wireInput, wireSwitch, setEnv, renderModels, renderAgent, resetForm } from './settings-form.js';

let mode = 'viewer';
let loadError = '';
let etag = null;
let draft = null;           // 编辑中的配置（edit 模式）
let dirty = false;
let saving = false;
let view = 'form';
let tomlSeq = 0;
const invalid = new Set();  // 当前格式不合法的输入框

export const SettingsPage = {
  init() {
    document.getElementById('tab-form').addEventListener('click', () => switchView('form'));
    document.getElementById('tab-toml').addEventListener('click', () => switchView('toml'));
    document.getElementById('save-btn').addEventListener('click', save);
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
  paintNotes(keepNotice);
  paintSave();
  if (view === 'toml') loadToml();
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
  const ctx = ctxOf(false);
  const modelsBox = document.getElementById('models-body');
  const agentBox = document.getElementById('agent-body');
  const [models, agents] = await Promise.allSettled([
    mode === 'nofile' ? api('/api/models') : Promise.reject(new ApiError(403, 'admin only')),
    api('/api/agents'),
  ]);
  // 只读呈现：limits 取自 GET /api/agents；服务 / 认证 / 存储 / 记忆无数据源，保持占位
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

/** index.html 骨架里的静态字段：data-cfg（输入）/ data-cfg-switch（开关）/ data-cfg-env（变量名）。 */
function bindStatic(cfg, ctx) {
  const root = document.getElementById('form-view');
  root.querySelectorAll('[data-cfg]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfg);
    wireInput(el, ctx, obj, prop, { type: el.dataset.type || 'str', nullable: 'nullable' in el.dataset });
  });
  root.querySelectorAll('[data-cfg-switch]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfgSwitch);
    wireSwitch(el, ctx, obj, prop);
  });
  root.querySelectorAll('[data-cfg-env]').forEach((el) => {
    const [obj, prop] = resolve(cfg, el.dataset.cfgEnv);
    setEnv(el, obj ? obj[prop] : null);
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

// ---------------------------------------------------------------- 卡片标注 / 顶部提示 / 保存按钮

/** 卡片头右侧小字。edit 模式下不加标注（生效方式由保存结果给出）。 */
function noteFor(card) {
  if (mode === 'edit') return card === 'memory' && draft?.memory == null ? '未启用 · 无 [memory] 节' : '';
  if (mode === 'nofile') {
    if (card === 'models' || card === 'agent' || card === 'limits') return '只读 · 服务未从配置文件启动';
    return '服务未从配置文件启动';
  }
  if (card === 'agent' || card === 'limits') return '只读';
  return mode === 'error' ? '加载失败' : '仅管理员可见';
}

function paintNotes(keepNotice) {
  document.querySelectorAll('#form-view [data-note]').forEach((el) => { el.textContent = noteFor(el.dataset.note); });
  if (keepNotice) return;
  if (mode === 'error') notice(`配置加载失败：${loadError}`, 'err');
  else if (mode === 'nofile') notice('服务未从配置文件启动，配置不可读写');
  else notice('部分修改需重启后生效');
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
    ? (dirty ? '写回 thought.toml（PUT /api/config）' : '没有未保存的修改')
    : mode === 'viewer' ? '仅管理员可修改配置' : '当前不可写回配置';
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
    notice(savedMessage(res), 'ok');
    refreshAgents();
  } catch (e) {
    saving = false;
    paintSave();
    if (e instanceof ApiError && e.status === 412) {
      notice('配置已被其他人修改，请重新加载后再保存', 'err');
      if (window.confirm('配置已在别处被修改（412）。\n重新加载最新配置？未保存的修改将丢失。')) await load();
    } else if (e instanceof ApiError && e.status === 422) {
      notice(`校验未通过：${e.message}`, 'err');
    } else {
      notice(`保存失败：${e.message}`, 'err');
    }
  }
}

function savedMessage(res) {
  const restart = res?.restart_required || [];
  const hot = res?.hot_applied || [];
  const lines = ['已保存到 thought.toml'];
  if (restart.length) lines.push(`需重启生效：${restart.join('、')}`);
  if (hot.length) lines.push(`已热生效：${hot.join('、')}（仅对新会话生效，进行中的会话不受影响）`);
  return lines.join('\n');
}

/** 保存后刷新任务页用到的 agent 配置缓存（worker chip / 并发上限）。 */
async function refreshAgents() {
  try { TasksStore.setAgents(await api('/api/agents')); } catch { /* 忽略 */ }
}

// ---------------------------------------------------------------- 表单 / 原始 TOML 切换

function switchView(v) {
  view = v;
  const toToml = v === 'toml';
  document.getElementById('form-view').style.display = toToml ? 'none' : 'block';
  document.getElementById('toml-view').style.display = toToml ? 'block' : 'none';
  document.getElementById('tab-form').classList.toggle('active', !toToml);
  document.getElementById('tab-toml').classList.toggle('active', toToml);
  if (toToml) loadToml();
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
    else if (e instanceof ApiError && e.status === 404) pre.textContent = '服务未从配置文件启动，没有可显示的 thought.toml';
    else pre.textContent = `加载失败：${e.message}`;
    return;
  }
  if (seq !== tomlSeq) return;
  paintToml(pre, text, dirty);
}

/** 轻量高亮：注释 / [节] / 键名。只读。 */
function paintToml(pre, text, hasDraft) {
  pre.textContent = '';
  const head = ['# 只读视图 · 已保存的 thought.toml（密钥字段已省略）'];
  if (hasDraft) head.push('# 表单中有未保存的修改，此处不包含');
  for (const line of [...head, '', ...text.split('\n')]) {
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
