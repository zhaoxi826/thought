// 设置页表单部件：输入 / 开关 / 逗号列表的接线，以及模型池卡与 worker 卡的构建。
// 由 js/settings.js 调用。编辑态下部件直接改写传入的配置对象（草稿），并回调 ctx.onChange。
// ctx = { editable, onChange(), invalid:Set<Element> }

import { h } from './lib/dom.js';

const PROVIDERS = ['openai', 'deepseek', 'anthropic']; // model::Provider（snake_case）
const openState = new Set();   // 展开的卡：`model:name` / `worker:name`，重绘后保持
let touched = false;           // 用户是否手动展开 / 收起过

/** 重新加载配置前调用。（暂无跨次状态要清；保留给 settings.js 的重置入口。） */
export function resetForm() {}

/** 输入值按类型转换：str / int / float；nullable 时空串 → null。 */
function parseValue(raw, type, nullable) {
  const s = raw.trim();
  if (type === 'str') return { ok: true, value: nullable && !s ? null : raw };
  if (!s) return nullable ? { ok: true, value: null } : { ok: false };
  const n = Number(s);
  if (!Number.isFinite(n) || (type === 'int' && (!Number.isInteger(n) || n < 0))) return { ok: false };
  return { ok: true, value: n };
}

/** 把一个 <input> 接到 obj[prop]。obj 为 null 或不可编辑时禁用。可重复调用（单一 oninput 槽）。 */
export function wireInput(el, ctx, obj, prop, { type = 'str', nullable = false } = {}) {
  const v = obj ? obj[prop] : null;
  const editable = ctx.editable && obj != null;
  el.type = type === 'str' ? 'text' : 'number';
  if (type !== 'str') el.classList.add('num');
  if (type === 'float') el.step = 'any';
  // f32 经 JSON 往返后的精度尾巴（0.6 → 0.6000000238…）只影响显示，不影响写回
  const shown = type === 'float' && typeof v === 'number' ? Math.round(v * 1e6) / 1e6 : v;
  el.value = shown ?? '';
  el.placeholder = '—';
  el.classList.toggle('placeholder', !editable && (v == null || v === ''));
  el.removeAttribute('aria-invalid');
  ctx.invalid.delete(el);
  if (!el.hasAttribute('aria-label')) el.setAttribute('aria-label', prop);
  el.disabled = !editable;
  el.oninput = editable ? () => {
    const r = el.validity?.badInput ? { ok: false } : parseValue(el.value, type, nullable);
    if (r.ok) { obj[prop] = r.value; ctx.invalid.delete(el); el.removeAttribute('aria-invalid'); }
    else { ctx.invalid.add(el); el.setAttribute('aria-invalid', 'true'); }
    ctx.onChange();
  } : null;
}

/** 把一个 .switch 接到布尔 obj[prop]（键盘空格 / 回车可切换）。 */
export function wireSwitch(sw, ctx, obj, prop, onFlip) {
  const editable = ctx.editable && obj != null;
  const paint = () => {
    const on = !!obj?.[prop];
    sw.classList.toggle('on', on);
    sw.setAttribute('aria-checked', String(on));
  };
  sw.setAttribute('role', 'switch');
  if (!sw.hasAttribute('aria-label')) {
    sw.setAttribute('aria-label', sw.parentElement?.querySelector('.sw-label')?.textContent || prop);
  }
  paint();
  sw.classList.toggle('readonly', !editable);
  sw.tabIndex = editable ? 0 : -1;
  sw.setAttribute('aria-disabled', String(!editable));
  const flip = () => { obj[prop] = !obj[prop]; paint(); onFlip?.(); ctx.onChange(); };
  sw.onclick = editable ? flip : null;
  sw.onkeydown = editable ? (e) => { if (e.key === ' ' || e.key === 'Enter') { e.preventDefault(); flip(); } } : null;
}

// ---------------------------------------------------------------- 字段构建

/** 标签 + 输入（label 的 for 指向输入，读屏可读）。 */
export function field(ctx, label, obj, prop, opts = {}) {
  const id = `cfg-${Math.random().toString(36).slice(2, 9)}`;
  const input = h('input', { id, 'aria-label': `${label} ${opts.key || prop}` });
  wireInput(input, ctx, obj, prop, opts);
  return h('div', { class: `field${opts.span2 ? ' span2' : ''}` },
    h('label', { for: id }, label, h('span', { class: 'key', text: opts.key || prop })),
    input,
  );
}

function textArea(ctx, label, obj, prop, minRows) {
  const id = `cfg-${Math.random().toString(36).slice(2, 9)}`;
  const text = obj[prop] || '';
  const rows = Math.min(Math.max(minRows, text.split('\n').length), 12);
  const ta = h('textarea', { id, rows: String(rows), disabled: ctx.editable ? null : '' });
  ta.value = text;
  if (ctx.editable) ta.oninput = () => { obj[prop] = ta.value; ctx.onChange(); };
  return h('div', { class: 'field span2' },
    h('label', { for: id }, label, h('span', { class: 'key', text: prop })),
    ta,
  );
}

/** 字符串数组字段：逗号分隔的单个输入（tools 等）。 */
function listField(ctx, label, obj, prop, opts = {}) {
  const id = `cfg-${Math.random().toString(36).slice(2, 9)}`;
  const input = h('input', { id, type: 'text', 'aria-label': `${label} ${prop}` });
  input.value = (obj[prop] || []).join(', ');
  input.disabled = !ctx.editable;
  if (ctx.editable) {
    input.oninput = () => {
      obj[prop] = input.value.split(',').map((s) => s.trim()).filter(Boolean);
      ctx.onChange();
    };
  }
  return h('div', { class: `field${opts.span2 ? ' span2' : ''}` },
    h('label', { for: id }, label, h('span', { class: 'key', text: prop })),
    input,
  );
}

function toggle(ctx, label, obj, prop, onFlip) {
  const sw = h('span', { class: 'switch', 'aria-label': label });
  wireSwitch(sw, ctx, obj, prop, onFlip);
  return h('div', { class: 'field' },
    h('div', { class: 'switch-row' },
      sw,
      h('span', { class: 'sw-label', text: label }),
      h('span', { class: 'sw-key', text: prop }),
    ),
  );
}

/** 可折叠卡（模型 / worker 共用设计稿的 .model 结构）。默认展开第一张，用户操作后记住。 */
function card(stateKey, idx, cls, headChildren, body) {
  if (!touched && idx === 0) openState.add(stateKey);
  const el = h('div', { class: `${cls}${openState.has(stateKey) ? ' open' : ''}` });
  const head = h('div', {
    class: 'model-head', role: 'button', tabindex: '0',
    'aria-expanded': String(openState.has(stateKey)),
  }, ...headChildren, h('span', { class: 'chev', text: '▸' }));
  const flip = () => {
    touched = true;
    const open = el.classList.toggle('open');
    head.setAttribute('aria-expanded', String(open));
    if (open) openState.add(stateKey); else openState.delete(stateKey);
  };
  head.addEventListener('click', flip);
  head.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); flip(); } });
  el.append(head, h('div', { class: 'model-body' }, body));
  return el;
}

// ---------------------------------------------------------------- 模型池

/** 渲染模型池。models = { name: ModelConfig }（编辑态为草稿对象，原地修改）。 */
export function renderModels(box, models, ctx) {
  box.textContent = '';
  const names = Object.keys(models || {});
  if (!names.length) box.append(h('div', { class: 'card-note', text: '模型池为空' }));
  names.forEach((name, i) => box.append(modelCard(name, models[name], i, ctx)));
  box.append(h('button', {
    class: 'add-model', type: 'button',
    disabled: ctx.editable ? null : '',
    onclick: ctx.editable ? () => addModel(box, models, ctx) : null,
  }, '+ 添加模型'));
}

function addModel(box, models, ctx) {
  const name = (window.prompt('新模型名（模型池 key，如 deepseek-flash）') || '').trim();
  if (!name) return;
  if (!/^[A-Za-z0-9_.-]+$/.test(name)) { window.alert('模型名只能包含字母、数字、. _ -'); return; }
  if (models[name]) { window.alert(`模型「${name}」已存在`); return; }
  models[name] = { provider: 'openai', model: '', base_url: '', api_key_env: null };
  openState.add(`model:${name}`);
  touched = true;
  ctx.onChange();
  renderModels(box, models, ctx);
}

function modelCard(name, cfg, idx, ctx) {
  const meta = h('span', { class: 'model-meta' });
  const badges = h('span', {});
  const paintHead = () => {
    meta.textContent = `${cfg.provider || ''} · ${cfg.model || ''}`;
    badges.textContent = '';
    if (cfg.thinking === false) badges.append(h('span', { class: 'badge', text: 'thinking = false' }));
    if (cfg.temperature != null) badges.append(h('span', { class: 'badge', text: `temp ${cfg.temperature}` }));
  };
  paintHead();
  const sub = { ...ctx, onChange() { paintHead(); ctx.onChange(); } };
  const ed = ctx.editable;
  const fields = h('div', { class: 'fields' },
    providerField(sub, cfg),
    field(sub, '模型', cfg, 'model'),
    ed || cfg.base_url ? field(sub, 'Base URL', cfg, 'base_url') : null,
    // api_key_env 是环境变量名（普通文本），密钥本体从不经过前端
    field(sub, 'API 密钥', cfg, 'api_key_env', { nullable: true, span2: !(ed || cfg.base_url) }),
    ed || cfg.temperature != null ? field(sub, '温度', cfg, 'temperature', { type: 'float', nullable: true }) : null,
    ed || cfg.top_p != null ? field(sub, 'Top P', cfg, 'top_p', { type: 'float', nullable: true }) : null,
    ed || cfg.max_tokens != null ? field(sub, '最大输出 tokens', cfg, 'max_tokens', { type: 'int', nullable: true }) : null,
    ed || cfg.thinking != null ? thinkingField(sub, cfg) : null,
  );
  return card(`model:${name}`, idx, 'model',
    [h('span', { class: 'model-name', text: name }), meta, badges], fields);
}

function providerField(ctx, cfg) {
  if (!ctx.editable) return field(ctx, '提供商', cfg, 'provider');
  const id = `cfg-${Math.random().toString(36).slice(2, 9)}`;
  const sel = h('select', { id }, ...PROVIDERS.map((p) => h('option', { value: p, text: p })));
  if (cfg.provider && !PROVIDERS.includes(cfg.provider)) sel.append(h('option', { value: cfg.provider, text: cfg.provider }));
  sel.value = cfg.provider || 'openai';
  sel.onchange = () => { cfg.provider = sel.value; ctx.onChange(); };
  return h('div', { class: 'field' },
    h('label', { for: id }, '提供商', h('span', { class: 'key', text: 'provider' })),
    sel,
  );
}

/** thinking：可选布尔（缺省 = 提供商默认），用三态下拉呈现。 */
function thinkingField(ctx, cfg) {
  if (!ctx.editable) {
    return field(ctx, '思考模式', { thinking: cfg.thinking == null ? '' : String(cfg.thinking) }, 'thinking');
  }
  const id = `cfg-${Math.random().toString(36).slice(2, 9)}`;
  const sel = h('select', { id },
    h('option', { value: '', text: '默认' }),
    h('option', { value: 'true', text: '开启' }),
    h('option', { value: 'false', text: '关闭' }),
  );
  sel.value = cfg.thinking == null ? '' : String(cfg.thinking);
  sel.onchange = () => {
    cfg.thinking = sel.value === '' ? null : sel.value === 'true';
    ctx.onChange();
  };
  return h('div', { class: 'field' },
    h('label', { for: id }, '思考模式', h('span', { class: 'key', text: 'thinking' })),
    sel,
  );
}

// ---------------------------------------------------------------- Agent

/** 渲染主控 + worker 卡。agent = { controller, workers }（编辑态为草稿，原地修改）。 */
export function renderAgent(box, agent, ctx) {
  box.textContent = '';
  if (agent.controller) {
    box.append(h('div', { class: 'fields mb14' },
      field(ctx, '主控模型', agent.controller, 'model', { key: 'controller.model' }),
      field(ctx, '主控轮数', agent.controller, 'max_turns', { key: 'controller.max_turns', type: 'int' }),
    ));
    if (agent.controller.system != null || ctx.editable) {
      box.append(h('div', { class: 'fields mb14' },
        textArea(ctx, '主控附加提示', nullableWrap(agent.controller), 'system', 2),
      ));
    }
    if (ctx.editable) {
      box.append(h('div', { class: 'fields mb14' },
        listField(ctx, '主控工具', agent.controller, 'tools', { key: 'controller.tools', span2: true }),
      ));
    }
  }
  const workers = agent.workers || [];
  if (!workers.length) box.append(h('div', { class: 'card-note', text: '未配置 worker' }));
  workers.forEach((w, i) => box.append(workerCard(w, i, ctx)));
}

/** controller.system 是 Option<String>：包一层把空串写成 null。 */
function nullableWrap(obj) {
  return new Proxy(obj, {
    get: (t, p) => t[p],
    set: (t, p, v) => { t[p] = v === '' ? null : v; return true; },
  });
}

function workerCard(w, idx, ctx) {
  const meta = h('span', { class: 'model-meta' });
  const spawn = h('span', {});
  const paintHead = () => {
    meta.textContent = w.model || '';
    spawn.className = `spawn-state${w.can_spawn ? ' yes' : ''}`;
    spawn.textContent = '';
    spawn.append(h('span', { class: 'mini-dot' }), w.can_spawn ? '可派生子 agent' : '不可派生');
  };
  paintHead();
  const sub = { ...ctx, onChange() { paintHead(); ctx.onChange(); } };
  const tools = w.tools || [];
  const body = h('div', { class: 'fields' },
    textArea(sub, '职责描述', w, 'description', 2),
    // system：/api/agents 与 /api/config 均带出；老后端缺字段时不显示
    w.system != null ? textArea(sub, '系统提示', w, 'system', 3) : null,
    ctx.editable
      ? listField(sub, '工具', w, 'tools', { span2: true })
      : h('div', { class: 'field span2' },
          h('label', {}, '工具', h('span', { class: 'key', text: 'tools' })),
          h('div', { class: 'tool-chips' }, ...tools.map((t) => h('span', { class: 'tchip', text: t }))),
        ),
    toggle(sub, '允许派生子 agent', w, 'can_spawn'),
    field(sub, '模型', w, 'model'),
    field(sub, '轮数上限', w, 'max_turns', { type: 'int' }),
  );
  return card(`worker:${w.name}`, idx, 'model worker', [
    h('span', { class: 'model-name', text: w.name }),
    meta,
    h('span', { class: 'tools-count', text: `tools ${tools.length} 个` }),
    spawn,
  ], body);
}
