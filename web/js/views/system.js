// 系统视图：agents 配置；admin 额外可见 models。

import { state } from '../state.js';
import { $, el } from '../util.js';
import { api } from '../lib/api.js';

export async function loadSystemView() {
  if (state.systemLoaded) return;
  state.systemLoaded = true;
  const epoch = state.epoch;
  const body = $('#system-body');
  body.textContent = '';
  const inner = el('div', 'system-body-inner');
  body.append(inner);
  const stale = () => epoch !== state.epoch || !inner.isConnected;

  // agents
  try {
    const agents = await api('/api/agents');
    if (stale()) return;
    inner.append(el('div', 'sys-section-title', 'AGENTS'));
    inner.append(agentCard('controller（主控）', { model: agents.controller.model, description: '接收消息、派发任务、汇总结果', tools: [], can_spawn: false }));
    for (const w of agents.workers || []) inner.append(agentCard(w.name, w));
  } catch (e) {
    if (stale()) return;
    state.systemLoaded = false; // 下次进入系统视图重试
    if (e.status !== 401) inner.append(el('div', 'sys-error', `加载 agents 失败：${e.message}`));
  }

  // models（仅 admin；403 隐藏）
  if (state.auth?.user?.role === 'admin') {
    try {
      const models = await api('/api/models');
      if (stale()) return;
      inner.append(el('div', 'sys-section-title', 'MODELS'));
      inner.append(modelsTable(models));
    } catch (e) {
      if (stale()) return;
      if (e.status !== 403 && e.status !== 401) inner.append(el('div', 'sys-error', `加载 models 失败：${e.message}`));
    }
  }
}

/** 登出 / 换用户：清空内容，下次进入重新加载。 */
export function resetSystemView() {
  state.systemLoaded = false;
  $('#system-body').textContent = '';
}

function agentCard(name, cfg) {
  const card = el('div', 'agent-card');
  const head = el('div', 'agent-card-head');
  head.append(el('span', 'agent-name', name));
  head.append(el('span', 'agent-model', cfg.model || ''));
  const spawn = el('span', `agent-spawn${cfg.can_spawn ? ' yes' : ''}`, cfg.can_spawn ? '可派生子 agent' : '不可派生');
  head.append(spawn);
  card.append(head);
  if (cfg.description) card.append(el('div', 'agent-desc', cfg.description));
  if (cfg.tools && cfg.tools.length) {
    const tools = el('div', 'agent-tools');
    for (const t of cfg.tools) tools.append(el('span', 'tool-chip', t));
    card.append(tools);
  }
  return card;
}

function modelsTable(models) {
  const table = el('table', 'models-table');
  const thead = el('tr');
  for (const h of ['名称', 'provider', 'model', 'base_url', '参数']) {
    const th = el('th', null, h);
    th.scope = 'col';
    thead.append(th);
  }
  table.append(thead);
  for (const [name, m] of Object.entries(models)) {
    const tr = el('tr');
    tr.append(el('td', 'mono', name));
    tr.append(el('td', 'mono', m.provider || ''));
    tr.append(el('td', 'mono', m.model || ''));
    tr.append(el('td', 'mono', m.base_url || ''));
    const params = ['temperature', 'top_p', 'max_tokens', 'thinking']
      .filter((k) => m[k] !== undefined && m[k] !== null)
      .map((k) => `${k}=${m[k]}`)
      .join(' ');
    tr.append(el('td', 'mono', params || '—'));
    table.append(tr);
  }
  return table;
}
