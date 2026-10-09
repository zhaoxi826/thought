// 设置页：三类数据 ——
//   1. 可真实展示：模型池（GET /api/models，admin；403 → 「仅管理员可见」）、
//      Agent（GET /api/agents），均为只读呈现（禁用输入态）；
//   2. 无数据源的分区（服务/认证/存储/记忆/限额）：index.html 里的骨架占位；
//   3. 「保存」禁用；「原始 TOML」态显示待配置 API 上线提示。
// 不装假数据。

import { api, ApiError } from './lib/api.js';
import { h } from './lib/dom.js';

export const SettingsPage = {
  init() {
    const tabForm = document.getElementById('tab-form');
    const tabToml = document.getElementById('tab-toml');
    tabForm.addEventListener('click', () => switchView('form'));
    tabToml.addEventListener('click', () => switchView('toml'));
    document.getElementById('toml-pre').textContent = '待配置 API（GET /api/config）上线后可用';
  },

  async start() {
    loadModels();
    loadAgents();
  },
};

function switchView(v) {
  const form = document.getElementById('form-view');
  const toml = document.getElementById('toml-view');
  const tf = document.getElementById('tab-form');
  const tt = document.getElementById('tab-toml');
  const toToml = v === 'toml';
  form.style.display = toToml ? 'none' : 'block';
  toml.style.display = toToml ? 'block' : 'none';
  tf.classList.toggle('active', !toToml);
  tt.classList.toggle('active', toToml);
}

// ---------------------------------------------------------------- 模型池

async function loadModels() {
  const box = document.getElementById('models-body');
  try {
    const models = await api('/api/models');
    box.textContent = '';
    const names = Object.keys(models || {});
    if (!names.length) {
      box.append(h('div', { class: 'card-note', text: '模型池为空' }));
      return;
    }
    names.forEach((name, i) => box.append(modelCard(name, models[name], i === 0)));
    box.append(h('button', {
      class: 'add-model', type: 'button', disabled: '',
      title: '待配置写回 API（PUT /api/config）',
    }, '+ 添加模型'));
    box.append(envNote('密钥通过环境变量注入'));
  } catch (e) {
    box.textContent = '';
    if (e instanceof ApiError && e.status === 403) {
      box.append(h('div', { class: 'card-note', text: '仅管理员可见' }));
    } else {
      box.append(h('div', { class: 'card-note', text: `加载失败：${e.message}` }));
    }
  }
}

function modelCard(name, cfg, open) {
  const meta = `${cfg.provider || ''} · ${cfg.model || ''}`;
  const badges = [];
  if (cfg.thinking === false) badges.push(h('span', { class: 'badge', text: 'thinking = false' }));
  if (cfg.temperature != null) badges.push(h('span', { class: 'badge', text: `temp ${cfg.temperature}` }));

  const fields = h('div', { class: 'fields' },
    roField('提供商', 'provider', cfg.provider),
    roField('模型', 'model', cfg.model),
    cfg.base_url ? roField('Base URL', 'base_url', cfg.base_url, true) : null,
    h('div', { class: `field${cfg.base_url ? '' : ' span2'}` },
      h('label', {}, 'API 密钥', h('span', { class: 'key', text: 'api_key_env' })),
      cfg.api_key_env
        ? h('div', { class: 'env-input' },
            h('span', { class: 'env-name', text: cfg.api_key_env }),
            lockIcon(),
          )
        : h('input', { type: 'text', class: 'placeholder', placeholder: '—', disabled: '' }),
    ),
    cfg.temperature != null ? roField('温度', 'temperature', cfg.temperature) : null,
    cfg.top_p != null ? roField('Top P', 'top_p', cfg.top_p) : null,
    cfg.max_tokens != null ? roField('最大输出 tokens', 'max_tokens', cfg.max_tokens) : null,
  );

  const card = h('div', { class: `model${open ? ' open' : ''}` },
    h('div', {
      class: 'model-head',
      onclick() { card.classList.toggle('open'); },
    },
      h('span', { class: 'model-name', text: name }),
      h('span', { class: 'model-meta', text: meta }),
      ...badges,
      h('span', { class: 'chev', text: '▸' }),
    ),
    h('div', { class: 'model-body' }, fields),
  );
  return card;
}

// ---------------------------------------------------------------- Agent

async function loadAgents() {
  const box = document.getElementById('agent-body');
  try {
    const agents = await api('/api/agents');
    box.textContent = '';
    box.append(h('div', { class: 'fields mb14' },
      h('div', { class: 'field' },
        h('label', {}, '主控模型', h('span', { class: 'key', text: 'controller.model' })),
        roInput(agents.controller?.model),
        h('div', { class: 'hint', text: '主控尽量只派单，仅开放记忆工具' }),
      ),
    ));
    (agents.workers || []).forEach((w, i) => box.append(workerCard(w, i === 0)));
  } catch (e) {
    box.textContent = '';
    box.append(h('div', { class: 'card-note', text: `加载失败：${e.message}` }));
  }
}

function workerCard(w, open) {
  const card = h('div', { class: `model worker${open ? ' open' : ''}` },
    h('div', {
      class: 'model-head',
      onclick() { card.classList.toggle('open'); },
    },
      h('span', { class: 'model-name', text: w.name }),
      h('span', { class: 'model-meta', text: w.model || '' }),
      h('span', { class: 'tools-count', text: `tools ${(w.tools || []).length} 个` }),
      h('span', { class: `spawn-state${w.can_spawn ? ' yes' : ''}` },
        h('span', { class: 'mini-dot' }),
        w.can_spawn ? '可派生子 agent' : '不可派生',
      ),
      h('span', { class: 'chev', text: '▸' }),
    ),
    h('div', { class: 'model-body' },
      h('div', { class: 'fields' },
        h('div', { class: 'field span2' },
          h('label', {}, '职责描述', h('span', { class: 'key', text: 'description' })),
          h('textarea', { rows: '2', disabled: '', text: w.description || '' }),
        ),
        h('div', { class: 'field span2' },
          h('label', {}, '工具', h('span', { class: 'key', text: 'tools' })),
          h('div', { class: 'tool-chips' },
            ...(w.tools || []).map((t) => h('span', { class: 'tchip', text: t })),
          ),
        ),
        h('div', { class: 'field' },
          h('div', { class: 'switch-row' },
            h('span', { class: `switch readonly${w.can_spawn ? ' on' : ''}` }),
            h('span', { class: 'sw-label', text: '允许派生子 agent' }),
            h('span', { class: 'sw-key', text: 'can_spawn' }),
          ),
        ),
        roField('模型', 'model', w.model),
      ),
    ),
  );
  return card;
}

// ---------------------------------------------------------------- 小部件

function roInput(value) {
  return h('input', { type: 'text', disabled: '', value: value ?? '' });
}

function roField(label, key, value, span2) {
  return h('div', { class: `field${span2 ? ' span2' : ''}` },
    h('label', {}, label, h('span', { class: 'key', text: key })),
    roInput(value),
  );
}

function lockIcon() {
  const span = h('span', { class: 'lock' });
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('width', '11'); svg.setAttribute('height', '11');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor'); svg.setAttribute('stroke-width', '1.8');
  const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
  rect.setAttribute('x', '4'); rect.setAttribute('y', '11');
  rect.setAttribute('width', '16'); rect.setAttribute('height', '10'); rect.setAttribute('rx', '2');
  const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  path.setAttribute('d', 'M8 11V7a4 4 0 0 1 8 0v4');
  svg.append(rect, path);
  span.append(svg);
  return span;
}

function envNote(text) {
  const note = h('div', { class: 'env-note' });
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('width', '11'); svg.setAttribute('height', '11');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', 'currentColor'); svg.setAttribute('stroke-width', '1.6');
  const c = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
  c.setAttribute('cx', '12'); c.setAttribute('cy', '12'); c.setAttribute('r', '9');
  const p = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  p.setAttribute('d', 'M12 8v5M12 16h.01');
  svg.append(c, p);
  note.append(svg, document.createTextNode(text));
  return note;
}
