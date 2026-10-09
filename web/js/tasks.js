// 任务页 UI（深色科技风）。数据全部来自 js/tasks-store.js 适配层（后端任务注册表）；
// 工具卡渲染在 js/tasks-activity.js。

import { TasksStore, runningByWorker, controllerBusy, busiestSessionRunning } from './tasks-store.js';
import { toolEvent, openCards, setRerender } from './tasks-activity.js';
import { h, fmtClockS, fmtRel, fmtDur, fmtNum } from './lib/dom.js';

let switchPage = () => {};
let filter = 'all';
let selectedKey = null;
let renderTimer = null;
let toastTimer = null;
const cancelling = new Set();    // 已发出取消请求、等待 task_finished 的任务

const STATUS_LABEL = { running: '进行中', done: '已完成', error: '失败', cancelled: '已取消' };
const STATUS_META = { running: 'RUN', done: 'OK', error: 'ERR', cancelled: 'CXL' };
const STATUS_DOT = { running: 'running', done: 'done', error: 'failed', cancelled: 'canceled' };
const GAUGE_LEN = 56.5;          // 2πr，r = 9

export const TasksPage = {
  init({ switchPage: sp }) {
    switchPage = sp;
    setRerender(render);
    document.querySelectorAll('#task-tabs .tab').forEach((tab) => {
      tab.addEventListener('click', () => {
        document.querySelectorAll('#task-tabs .tab').forEach((t) => t.classList.remove('active'));
        tab.classList.add('active');
        filter = tab.dataset.filter;
        render();
      });
    });
    TasksStore.onChange(scheduleRender);
    setInterval(tick, 1000);
    render();
  },

  /** 从对话页「查看任务 →」跳入：自动选中对应任务。 */
  focusTask(key) {
    filter = 'all';
    document.querySelectorAll('#task-tabs .tab').forEach((t) => t.classList.toggle('active', t.dataset.filter === 'all'));
    selectedKey = key;
    render();
    switchPage('tasks');
  },
};

function scheduleRender() {
  if (renderTimer) return;
  // text_delta 期间变化很密，节流整页重绘
  renderTimer = setTimeout(() => { renderTimer = null; render(); }, 300);
}

// ---------------------------------------------------------------- 渲染

function render() {
  const all = TasksStore.list();
  renderStatusBar(all);
  renderTabs(all);
  renderList(all);
  renderFoot(all);
  renderDetail();
}

function renderStatusBar(all) {
  const running = all.filter((t) => t.status === 'running').length;
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const doneToday = all.filter((t) => t.status === 'done' && t.finishedAt >= today.getTime()).length;
  const failed = all.filter((t) => t.status === 'error').length;

  const ov = document.getElementById('sb-overview');
  ov.textContent = '';
  ov.append(
    h('span', { class: 'live' }, '● 进行中 ', h('b', {}, String(running))),
    document.createTextNode(' · 今日完成 '),
    h('b', {}, String(doneToday)),
    document.createTextNode(' · 失败 '),
    h('b', {}, String(failed)),
  );

  // worker chips：配置来自 GET /api/agents，运行状态来自适配层
  const box = document.getElementById('sb-workers');
  box.textContent = '';
  const agents = TasksStore.agents;
  const busy = runningByWorker();
  const chips = [];
  if (agents) {
    const cBusy = controllerBusy();
    chips.push(chip('controller', agents.controller?.model || '', cBusy, null, cBusy ? '执行中' : '待命'));
    for (const w of agents.workers || []) {
      const list = busy.get(w.name) || [];
      chips.push(chip(w.name, w.model, list.length > 0, list[0]?.taskId, list.length > 0 ? '执行中' : '空闲'));
    }
  }
  // 配置未加载但有任务时，至少把忙碌 worker 显示出来
  if (!agents) {
    for (const [name, list] of busy) chips.push(chip(name, '', true, list[0]?.taskId, '执行中'));
  }
  chips.forEach((c) => box.append(c));

  // 并发仪表：N = agent.limits.max_concurrent_tasks（来自 GET /api/agents 的 limits）。
  // 上限按会话计，分子取进行中任务最多的那个会话；总进行中数见左侧概览。
  const limit = TasksStore.limits?.max_concurrent_tasks || 0;
  const inSession = busiestSessionRunning();
  const frac = limit ? Math.min(inSession / limit, 1) : (running > 0 ? 0.25 : 0);
  const gauge = h('div', {
    class: 'gauge',
    title: limit ? `单会话并发上限 ${limit}（agent.limits.max_concurrent_tasks）` : '并发上限未知',
  },
    h('svg', { width: '22', height: '22', viewBox: '0 0 24 24', fill: 'none' },
      svgCircle('#343a44', null),
      frac > 0 ? svgCircle('#e0574a', `${(GAUGE_LEN * frac).toFixed(1)} ${GAUGE_LEN}`) : null,
    ),
    h('span', { class: 'gv' }, h('b', {}, String(limit ? inSession : running)), limit ? ` / ${limit}` : null),
  );
  box.append(gauge);
}

function svgCircle(color, dash) {
  const c = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
  c.setAttribute('cx', '12'); c.setAttribute('cy', '12'); c.setAttribute('r', '9');
  c.setAttribute('stroke', color); c.setAttribute('stroke-width', '2.5');
  if (dash) {
    c.setAttribute('stroke-linecap', 'round');
    c.setAttribute('stroke-dasharray', dash);
    c.setAttribute('transform', 'rotate(-90 12 12)');
  }
  return c;
}

function chip(name, model, isBusy, taskId, stateText) {
  return h('div', { class: `chip${isBusy ? ' busy' : ''}` },
    h('span', { class: `wdot ${isBusy ? 'busy' : 'idle'}` }),
    h('span', { class: 'wname', text: name }),
    model ? h('span', { class: 'wmodel', text: model }) : null,
    h('span', { class: 'wstate', text: stateText }),
    isBusy && taskId != null ? h('span', { class: 'taskno', text: `#${taskId}` }) : null,
  );
}

function renderTabs(all) {
  const counts = {
    all: all.length,
    running: all.filter((t) => t.status === 'running').length,
    done: all.filter((t) => t.status === 'done').length,
    failed: all.filter((t) => t.status === 'error' || t.status === 'cancelled').length,
  };
  document.querySelectorAll('#task-tabs .tab').forEach((tab) => {
    const n = tab.querySelector('.n');
    if (n) n.textContent = String(counts[tab.dataset.filter] ?? 0);
  });
}

function matchFilter(t) {
  if (filter === 'all') return true;
  if (filter === 'failed') return t.status === 'error' || t.status === 'cancelled';
  return t.status === filter;
}

function renderList(all) {
  const box = document.getElementById('task-list');
  box.textContent = '';
  const shown = all.filter(matchFilter);
  if (!shown.length) {
    box.append(h('div', { class: 'list-empty', text: all.length ? '该分类下暂无任务' : '暂无任务 · 对话中主控派单后会出现在这里' }));
    return;
  }
  for (const t of shown) {
    if (!selectedKey) selectedKey = t.key;
    const item = h('div', {
      class: `task${t.key === selectedKey ? ' selected' : ''}${t.status === 'running' ? ' is-running' : ''}`,
      dataset: { status: t.status },
      onclick: () => { selectedKey = t.key; TasksStore.ensureDetail(t.key, true); render(); },
    },
      h('span', { class: `sdot ${STATUS_DOT[t.status]}` }),
      h('div', { class: 'task-main' },
        h('div', { class: 'task-top' },
          h('span', { class: 'who', text: t.label }),
          h('span', { class: 'ago', text: fmtRel(t.startedAt) }),
        ),
        h('div', { class: 'task-title', text: t.task }),
        h('div', { class: 'task-meta', dataset: { metaKey: t.key } }, ...metaNodes(t)),
      ),
    );
    box.append(item);
  }
}

/** 列表底栏：已载入条数 + 加载更多（游标分页）· 单会话并发。 */
function renderFoot(all) {
  const foot = document.getElementById('task-list-foot');
  if (!foot) return;
  foot.textContent = '';
  const left = h('span', { text: `LOADED ${all.length}` });
  if (TasksStore.hasMore()) {
    const busy = TasksStore.isLoadingMore();
    left.append(h('button', {
      class: 'more-btn foot-more', type: 'button', disabled: busy ? '' : null,
      onclick() {
        TasksStore.loadMore().catch((e) => toast(`加载失败：${e.message}`));
      },
    }, busy ? '加载中…' : '加载更多'));
  }
  const limit = TasksStore.limits?.max_concurrent_tasks;
  foot.append(left, h('span', { text: `CONCURRENCY ${busiestSessionRunning()}/${limit || '—'}` }));
}

/** 任务页右下角短暂提示（取消失败等）。 */
function toast(message) {
  let el = document.getElementById('tasks-toast');
  if (!el) {
    el = h('div', { class: 'toast', id: 'tasks-toast', role: 'status', 'aria-live': 'polite' });
    document.querySelector('.page-tasks').append(el);
  }
  el.textContent = message;
  el.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('show'), 4000);
}

async function cancelTask(t) {
  if (!window.confirm(`取消任务 ${t.label}？\n「${t.task.slice(0, 80)}」`)) return;
  cancelling.add(t.key);
  render();
  try {
    await TasksStore.cancel(t.key);
    // 204：随后由 task_finished { error: "cancelled" } 落定状态
  } catch (e) {
    cancelling.delete(t.key);
    if (e?.status === 409) toast('任务已结束，无法取消');
    else if (e?.status === 404) toast('任务不存在（可能已被清理）');
    else toast(`取消失败：${e.message}`);
    render();
  }
}

function metaNodes(t) {
  const st = t.status === 'running' || t.status === 'error'
    ? h('span', { class: t.status === 'running' ? 'st-running' : 'st-failed', text: STATUS_META[t.status] })
    : document.createTextNode(STATUS_META[t.status]);
  return [st, document.createTextNode(` · ${fmtDur(elapsed(t))} · ${fmtNum(t.tokens)} tok`)];
}

function elapsed(t) {
  return (t.finishedAt || Date.now()) - t.startedAt;
}

// ---------------------------------------------------------------- 详情

function renderDetail() {
  const box = document.getElementById('task-detail');
  const t = selectedKey && TasksStore.get(selectedKey);
  if (!t) {
    box.textContent = '';
    box.append(h('div', { class: 'detail-empty', text: TasksStore.list().length ? '选择左侧任务查看详情' : '暂无任务' }));
    return;
  }
  // 回填持久化活动事件（幂等：已加载 / 加载中不重复请求；结束后标 stale 会再拉一次补齐末段）
  TasksStore.ensureDetail(t.key);
  if (t.status !== 'running') cancelling.delete(t.key);
  const scroll = box.querySelector('.scroll');
  const keepScroll = scroll ? scroll.scrollTop : 0;
  box.textContent = '';
  box.append(detailHead(t), detailBody(t));
  const ns = box.querySelector('.scroll');
  if (ns) ns.scrollTop = keepScroll;
}

function workerSpec(t) {
  return (TasksStore.agents?.workers || []).find((w) => w.name === t.worker) || null;
}

function detailHead(t) {
  const spec = workerSpec(t);
  return h('div', { class: 'task-head' },
    h('div', {},
      h('span', { class: 'th-name', text: t.label }),
      h('span', { class: `badge ${STATUS_DOT[t.status] === 'failed' ? 'failed' : STATUS_DOT[t.status]}` },
        t.status === 'running' ? h('i', {}) : null,
        STATUS_LABEL[t.status],
      ),
      h('div', { class: 'th-sub' },
        document.createTextNode('模型 '),
        h('span', { class: 'model', text: spec?.model || '—' }),
        document.createTextNode(` · 由 controller 派单 · 会话「${t.sessionTitle || t.sessionId}」`),
      ),
    ),
    h('div', { class: 'th-stats' },
      h('div', { class: 'stat' },
        h('span', { class: 'k', text: 'Elapsed' }),
        h('span', { class: 'v', text: fmtDur(elapsed(t)), dataset: { statElapsed: t.status === 'running' ? t.key : '' } }),
      ),
      h('div', { class: 'stat' },
        h('span', { class: 'k', text: 'Tokens' }),
        h('span', { class: 'v', text: fmtNum(t.tokens) }),
      ),
      // 取消：POST /api/tasks/{session_id}/{task_id}/cancel；仅进行中可点
      cancelButton(t),
    ),
  );
}

function cancelButton(t) {
  if (t.status !== 'running') {
    return h('button', { class: 'cancel', type: 'button', disabled: '', title: '任务已结束' }, '取消');
  }
  if (cancelling.has(t.key)) {
    return h('button', { class: 'cancel', type: 'button', disabled: '', title: '已请求取消，等待任务结束' }, '取消中…');
  }
  return h('button', { class: 'cancel', type: 'button', onclick: () => cancelTask(t) }, '取消');
}

function emptyActivityText(t) {
  if (t.detail === 'loading' || t.detail === 'none') return '加载活动记录……';
  if (t.detail === 'error') return '活动记录加载失败';
  return t.status === 'running' ? '等待第一个活动事件……' : '无活动记录';
}

function detailBody(t) {
  const spec = workerSpec(t);
  const col = h('div', { class: 'column' },
    h('div', { class: 'sec-label', text: 'Task' }),
    h('div', { class: 'desc' },
      h('div', { class: 'body', text: t.task }),
      h('div', { class: 'foot' },
        h('span', { text: `DISPATCHED ${fmtClockS(t.startedAt)}` }),
        h('span', { text: spec ? (spec.can_spawn ? 'SPAWN ALLOWED' : 'SPAWN DENIED') : 'SPAWN —' }),
        h('span', { text: `TOOLS ${spec ? spec.tools.length : '—'}` }),
      ),
    ),
    h('div', { class: 'sec-label', text: 'Activity' }),
  );
  const stream = h('div', { class: 'stream' });
  if (!t.events.length) {
    stream.append(h('div', { class: 'list-empty', text: emptyActivityText(t) }));
  }
  t.events.forEach((ev, i) => stream.append(renderEvent(t, ev, i)));
  col.append(stream);

  // 结果 / 错误卡片
  if (t.status !== 'running') {
    col.append(h('div', { class: 'sec-label', text: 'Result / Error' }));
    col.append(h('div', { class: 'ev plain flush' }, resultCard(t)));
  }
  return h('div', { class: 'scroll' }, col);
}

function renderEvent(t, ev, idx) {
  if (ev.kind === 'text') {
    return h('div', { class: 'ev' },
      h('div', { class: 'ev-head' },
        h('span', { class: 'ev-tag', text: 'AGENT' }),
        h('span', { class: 'ev-ts', text: fmtClockS(ev.ts) }),
      ),
      h('div', { class: 'agent-line' }, h('span', { class: 'agent-who', text: ev.agent })),
      h('div', { class: 'agent-body', text: ev.text, dataset: { agentBody: `${t.key}:${idx}` } }),
    );
  }
  if (ev.kind === 'sub') {
    const sub = ev.sub;
    const done = t.status !== 'running';
    return h('div', { class: 'ev' },
      h('div', { class: 'ev-head' },
        h('span', { class: 'ev-tag', text: 'SUB' }),
        h('span', { class: 'ev-ts', text: fmtClockS(ev.ts) }),
      ),
      h('div', { class: 'sub' },
        h('div', { class: 'sub-head' },
          h('span', { class: 'role', text: sub.name }),
          h('span', { class: 'chip2', text: `SUB-AGENT · ${sub.worker.toUpperCase()}` }),
          sub.note ? h('span', { class: 'note', text: sub.note }) : null,
          done ? h('span', { class: 'ok', text: '✓' }) : null,
        ),
        ...sub.events.map((se, i) => {
          if (se.kind === 'tool') return toolEvent(t, se.call, true);
          if (se.kind === 'text') {
            return h('div', { class: 'ev sub-text' },
              h('div', { class: 'agent-line' }, h('span', { class: 'agent-who', text: se.agent })),
              h('div', { class: 'agent-body', text: se.text }),
            );
          }
          return h('span', {});
        }),
      ),
    );
  }
  // kind === 'tool'
  return toolEvent(t, ev.call, false);
}

function resultCard(t) {
  const failed = t.status === 'error' || t.status === 'cancelled';
  const cardKey = `${t.key}:result`;
  const card = h('div', {
    class: `tc result-card ${failed ? 'is-failed' : 'is-done'}`,
    onclick(e) {
      if (e.target.closest('.tc-body')) return;
      card.classList.toggle('open');
      if (card.classList.contains('open')) openCards.add(cardKey); else openCards.delete(cardKey);
    },
  },
    h('div', { class: 'tc-head' },
      h('span', { class: 'rc-tag', text: t.status === 'done' ? 'Result' : t.status === 'cancelled' ? 'Cancelled' : 'Error' }),
      h('span', { class: 'rc-sum', text: t.task }),
      h('div', { class: 'right' }, h('span', { class: 'chev', text: '▸' })),
    ),
    h('div', { class: 'tc-body' },
      failed
        ? h('div', { class: 'err-code', text: t.error || '' })
        : h('div', {},
            h('div', { class: 'rc-body-text', text: t.result || '（无结果文本）' }),
            h('div', { class: 'code' },
              h('span', { class: 'dim', text: 'elapsed' }),
              document.createTextNode(` ${fmtDur(elapsed(t))} · `),
              h('span', { class: 'dim', text: 'tokens' }),
              document.createTextNode(` ${fmtNum(t.tokens)}`),
            ),
          ),
    ),
  );
  if (openCards.has(cardKey)) card.classList.add('open');
  return card;
}

// ---------------------------------------------------------------- 每秒 tick：只更新耗时显示，不重绘

function tick() {
  const now = Date.now();
  let anyRunning = false;
  for (const t of TasksStore.list()) {
    if (t.status !== 'running') continue;
    anyRunning = true;
    const meta = document.querySelector(`[data-meta-key="${CSS.escape(t.key)}"]`);
    if (meta) {
      meta.textContent = '';
      meta.append(...metaNodes(t));
    }
  }
  document.querySelectorAll('[data-stat-elapsed]').forEach((el) => {
    const key = el.dataset.statElapsed;
    if (!key) return;
    const t = TasksStore.get(key);
    if (t) el.textContent = fmtDur(elapsed(t));
  });
  document.querySelectorAll('[data-live-since]').forEach((el) => {
    el.textContent = `RUNNING ${fmtDur(now - Number(el.dataset.liveSince))}`;
  });
  // rail 呼吸点
  document.getElementById('tasks-ping').hidden = !anyRunning;
}
