// 任务页活动流：工具卡（TOOL / CMD / DIFF / MEM）渲染。由 js/tasks.js 调用。
// 卡片展开 / 收起 / 全量输出状态跨重绘保留在本模块。

import { h, fmtClockS, fmtDur } from './lib/dom.js';

export const openCards = new Set();     // 展开的卡：`${taskKey}:${agent}:${callId}`
const closedCards = new Set();          // 被手动收起的默认展开卡（DIFF）
const expandedOut = new Set();          // 展开全量输出的工具卡
let render = () => {};

/** 注册整页重绘函数（「展开全部」后需要重绘）。 */
export function setRerender(fn) { render = fn; }

function toolTag(name) {
  if (name.startsWith('memory_')) return 'MEM';
  if (name === 'run_command') return 'CMD';
  if (name === 'write_file' || name === 'edit_file') return 'DIFF';
  return 'TOOL';
}

export function toolEvent(t, call, nested) {
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
  // call id 只保证在单个 agent 内唯一，子 agent 之间可能重复
  const cardKey = `${t.key}:${call.agent}:${call.id}`;
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
