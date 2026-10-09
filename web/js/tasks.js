// 任务页 UI（深色科技风）。数据全部来自 js/tasks-store.js 适配层，
// 未来后端任务注册表上线后本文件不需要改动。

import { TasksStore, runningByWorker, controllerBusy } from './tasks-store.js';
import { h, fmtClockS, fmtRel, fmtDur, fmtNum } from './lib/dom.js';

let switchPage = () => {};
let filter = 'all';
let selectedKey = null;
const openCards = new Set();     // 展开的工具卡：`${taskKey}:${callId}`
const closedCards = new Set();   // 被手动收起的默认展开卡（DIFF）
const expandedOut = new Set();   // 展开全量输出的工具卡
let renderTimer = null;

const STATUS_LABEL = { running: '进行中', done: '已完成', error: '失败', cancelled: '已取消' };
const STATUS_META = { running: 'RUN', done: 'OK', error: 'ERR', cancelled: 'CXL' };
const STATUS_DOT = { running: 'running', done: 'done', error: 'failed', cancelled: 'canceled' };

export const TasksPage = {
  init({ switchPage: sp }) {
    switchPage = sp;
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

  // 并发仪表：N（并发上限）无从得知，先只显示进行中数。
  // TODO(backend): GET /api/config 上线后读 agent.limits.max_concurrent_tasks 补全 N，见 design/backend-api.md
  const gauge = h('div', { class: 'gauge', title: '并发上限待后端配置 API（GET /api/config）' },
    h('svg', { width: '22', height: '22', viewBox: '0 0 24 24', fill: 'none' },
      svgCircle('#343a44', null),
      running > 0 ? svgCircle('#e0574a', '14.1 56.5') : null,
    ),
    h('span', { class: 'gv' }, h('b', {}, String(running))),
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
      onclick: () => { selectedKey = t.key; render(); },
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
      // 取消：无 API，禁用态。后端 POST /api/tasks/{task_id}/cancel 落地后放开。
      h('button', { class: 'cancel', disabled: '', title: '待后端支持（POST /api/tasks/{task_id}/cancel）' }, '取消'),
    ),
  );
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
    stream.append(h('div', { class: 'list-empty', text: t.status === 'running' ? '等待第一个活动事件……' : '无活动记录（任务在页面打开前已开始）' }));
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

function toolTag(name) {
  if (name.startsWith('memory_')) return 'MEM';
  if (name === 'run_command') return 'CMD';
  if (name === 'write_file' || name === 'edit_file') return 'DIFF';
  return 'TOOL';
}

function toolEvent(t, call, nested) {
  const tag = toolTag(call.name);
  const running = call.status === 'running';
  const ev = h('div', { class: `ev${tag === 'DIFF' ? ' key' : ''}${running ? ' live' : ''}` },
    h('div', { class: 'ev-head' },
      h('span', { class: 'ev-tag', text: tag }),
      h('span', { class: 'ev-ts', text: fmtClockS(call.ts) }),
    ),
    toolCard(t, call, tag, nested),
  );
  return ev;
}

function toolCard(t, call, tag, nested) {
  const cardKey = `${t.key}:${call.id}`;
  const running = call.status === 'running';
  // DIFF 卡默认展开（设计稿如此），被手动收起后记住；运行中的卡强制展开
  const open = running || (tag === 'DIFF' ? !closedCards.has(cardKey) : openCards.has(cardKey));
  const args = parseArgs(call.arguments);

  const right = h('div', { class: 'right' });
  const ds = diffStat(call.name, args);
  if (ds) right.append(h('span', { class: 'diffstat' },
    h('span', { class: 'add', text: `+${ds.add}` }),
    ds.del ? h('span', { class: 'del', text: `−${ds.del}` }) : null,
  ));
  if (running) {
    right.append(
      h('span', { class: 'live-tag' }, h('i', {}), h('span', { text: `RUNNING ${fmtDur(Date.now() - call.ts)}`, dataset: { liveSince: String(call.ts) } })),
      spinSvg(),
    );
  } else {
    right.append(
      h('span', { class: 'dur', text: call.doneTs ? fmtDur(call.doneTs - call.ts) : '' }),
      h('span', { class: 'ok', text: '✓' }),
    );
  }
  right.append(h('span', { class: 'chev', text: '▸' }));

  const card = h('div', {
    class: `tc${open ? ' open' : ''}${running ? ' live' : ''}${nested ? '' : ''}`,
    onclick(e) {
      // 点卡片头以外区域（正文）不折叠
      if (e.target.closest('.tc-body') || e.target.closest('.more-btn')) return;
      card.classList.toggle('open');
      if (card.classList.contains('open')) { openCards.add(cardKey); closedCards.delete(cardKey); }
      else { openCards.delete(cardKey); closedCards.add(cardKey); }
    },
  },
    h('div', { class: 'tc-head' },
      h('span', { class: 'tool', text: call.name }),
      h('span', { class: 'target', text: targetOf(call.name, args, call.arguments) }),
      right,
    ),
    h('div', { class: 'tc-body' }, ...toolBody(t, call, args, tag, cardKey)),
  );
  return card;
}

function spinSvg() {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('class', 'spin');
  svg.setAttribute('width', '12'); svg.setAttribute('height', '12');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('fill', 'none');
  svg.setAttribute('stroke', '#e0574a'); svg.setAttribute('stroke-width', '2.4');
  svg.setAttribute('stroke-linecap', 'round');
  const p = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  p.setAttribute('d', 'M12 3a9 9 0 1 1-9 9');
  svg.append(p);
  return svg;
}

function toolBody(t, call, args, tag, cardKey) {
  const nodes = [];
  // DIFF：write_file 的 file_content 按设计稿 diff 视图呈现（全为新增行）；
  // 没有结构化 diff 时不硬编，退回等宽 pre 展示参数。
  if (tag === 'DIFF' && args && typeof args.file_content === 'string') {
    const target = args.file_path || '';
    nodes.push(h('div', { class: 'mini-label', text: `Content · ${target}` }));
    nodes.push(diffView(args.file_content));
  } else {
    nodes.push(h('div', { class: 'mini-label', text: 'Params' }));
    nodes.push(codeBlock(prettyArgs(call.arguments, args)));
  }
  if (call.output != null) {
    const isCmd = call.name === 'run_command';
    nodes.push(h('div', { class: 'mini-label', text: isCmd ? 'Output · Tail' : 'Output' }));
    nodes.push(outputBlock(call.output, isCmd, cardKey));
  }
  return nodes;
}

/** 大输出：默认截断（run_command 取尾部），点「展开全部」看全量。 */
function outputBlock(output, tailOnly, cardKey) {
  const LIMIT = 3000;
  let text = String(output);
  let truncated = false;
  if (!expandedOut.has(cardKey) && text.length > LIMIT) {
    truncated = true;
    text = tailOnly ? tail(text, 14) : text.slice(0, LIMIT);
  } else if (tailOnly && expandedOut.has(cardKey) === false && tailLines(text) > 14) {
    text = tail(text, 14);
    truncated = true;
  }
  const wrap = h('div', {});
  wrap.append(codeBlock(text + (truncated ? '\n…' : '')));
  if (truncated) {
    wrap.append(h('button', {
      class: 'more-btn', type: 'button',
      onclick() { expandedOut.add(cardKey); render(); },
    }, '展开全部'));
  }
  return wrap;
}

function tail(text, n) {
  const lines = text.split('\n');
  return lines.slice(Math.max(0, lines.length - n)).join('\n');
}
function tailLines(text) { return text.split('\n').length; }

function codeBlock(text) {
  return h('div', { class: 'code', text });
}

/** diff 视图：全部按新增行呈现（write_file 是全量写入）。 */
function diffView(content) {
  const box = h('div', { class: 'diff' });
  content.split('\n').forEach((line, i) => {
    box.append(h('span', { class: 'dl add' },
      h('span', { class: 'ln', text: String(i + 1) }),
      document.createTextNode(`+ ${line}`),
    ));
  });
  return box;
}

function diffStat(name, args) {
  if ((name === 'write_file' || name === 'edit_file') && args && typeof args.file_content === 'string') {
    return { add: args.file_content.split('\n').length, del: 0 };
  }
  return null;
}

function parseArgs(raw) {
  try { return JSON.parse(raw); } catch { return null; }
}

function prettyArgs(raw, parsed) {
  if (!parsed) return String(raw ?? '');
  return JSON.stringify(parsed, null, 2);
}

function targetOf(name, args, raw) {
  if (args) {
    const v = args.file_path ?? args.command ?? args.query ?? args.task ?? args.content ?? args.file_content;
    if (typeof v === 'string') return v.length > 80 ? `${v.slice(0, 80)}…` : v;
  }
  const s = String(raw ?? '');
  return s.length > 80 ? `${s.slice(0, 80)}…` : s;
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
