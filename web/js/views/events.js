// SSE 实时事件 → 消息区渲染。

import { state } from '../state.js';
import { el, fmtElapsed, announce } from '../util.js';
import {
  ensureRun, latestRun, parseAgent, taskFor, toolContainerFor, ensureTaskCard,
  addToolCard, fillToolOutput, replyBlock, addNotice, updateTasksBadge, scrollBottom,
} from './run.js';
import { loadSessions } from './sessions.js';

// 任务卡片内某个 agent（含子 agent）的流式区域
function agentStreamEl(taskCtx, label, isReasoning) {
  const key = `${label}${isReasoning ? '|r' : ''}`;
  if (taskCtx.agentStreams.has(key)) return taskCtx.agentStreams.get(key);
  const wrap = el('div', 'agent-sub');
  wrap.append(el('div', 'stream-label', label));
  const stream = el('div', isReasoning ? 'reasoning-stream streaming-cursor' : 'text-stream streaming-cursor');
  wrap.append(stream);
  taskCtx.bodyEl.append(wrap);
  taskCtx.agentStreams.set(key, stream);
  return stream;
}

function onDelta(ev, isReasoning) {
  const loc = parseAgent(ev.agent);
  let target;
  if (!loc) {
    const run = ensureRun(); // 上一块已 reply 完成时，这里会开新块
    target = isReasoning ? run.reasonEl : run.textEl;
    target.classList.remove('hidden');
  } else {
    target = agentStreamEl(taskFor(loc), ev.agent, isReasoning);
  }
  target.appendChild(document.createTextNode(ev.text));
  scrollBottom();
}

function onToolCall(ev) {
  addToolCard(toolContainerFor(ev.agent), { id: ev.id, name: ev.name, args: ev.arguments });
}

function onToolResult(ev) {
  let ctx = state.toolCards.get(ev.id);
  if (!ctx) {
    // 没见到对应调用（事件丢失等）：补一张只有结果的卡片
    ctx = addToolCard(toolContainerFor(ev.agent), { id: ev.id, name: ev.name, args: '' });
  }
  fillToolOutput(ctx, ev.output ?? '');
}

function onTaskStarted(ev) {
  ensureTaskCard(String(ev.task_id), ev.worker, ev.task);
}

function onTaskFinished(ev) {
  const taskId = String(ev.task_id);
  const ctx = state.taskCards.get(taskId);
  if (!ctx) return;
  const card = ctx.el;
  card.classList.remove('running');
  const start = Number(card.dataset.start || Date.now());
  ctx.elapsedEl.textContent = fmtElapsed(start);
  for (const stream of ctx.agentStreams.values()) stream.classList.remove('streaming-cursor');

  if (ev.error === 'cancelled') {
    card.classList.add('cancelled');
    ctx.statusEl.textContent = '⊘';
    ctx.statusEl.setAttribute('aria-label', '已取消');
  } else if (ev.error) {
    card.classList.add('failed');
    ctx.statusEl.textContent = '✗';
    ctx.statusEl.setAttribute('aria-label', '失败');
    ctx.bodyEl.append(el('div', 'task-result error', ev.error));
  } else {
    card.classList.add('done');
    ctx.statusEl.textContent = '✓';
    ctx.statusEl.setAttribute('aria-label', '完成');
    if (ev.result) ctx.bodyEl.append(el('div', 'task-result', ev.result));
  }
  state.runningTasks.delete(taskId);
  updateTasksBadge();
  scrollBottom();
}

function onUsage(ev) {
  const run = latestRun();
  run.usage.prompt += ev.usage?.prompt_tokens || 0;
  run.usage.completion += ev.usage?.completion_tokens || 0;
  const { prompt, completion } = run.usage;
  run.usageEl.textContent = `tokens ↑ ${prompt} ↓ ${completion}（共 ${prompt + completion}）`;
  run.usageEl.classList.remove('hidden');
}

function onReply(ev) {
  const run = ensureRun();
  // 流式区定型为 markdown 渲染的最终回复
  run.textEl.classList.remove('streaming-cursor');
  run.textEl.classList.add('hidden');
  run.reasonEl.classList.remove('streaming-cursor');
  if (!run.reasonEl.textContent) run.reasonEl.classList.add('hidden');
  run.el.insertBefore(replyBlock(ev.text), run.toolsEl);
  run.el.classList.remove('streaming');
  // 本块定型：同一 run 里主控若再回复，后续增量进新的运行块（不会追加到已隐藏的流式区）
  run.done = true;
  scrollBottom();
  announce(`thought 回复：${ev.text || ''}`);
  // 会话列表的 updated_at 变了，静默刷新
  loadSessions();
}

function onErrorEvent(ev) {
  const run = latestRun();
  run.el.append(el('div', 'run-error', `错误：${ev.message}`));
  scrollBottom();
  announce(`错误：${ev.message}`);
}

/** 分发一条 SSE 事件。 */
export function handleEvent(name, data) {
  if (name === 'lagged') {
    addNotice(`事件流滞后，丢失了 ${data} 条事件`);
    return;
  }
  let ev;
  try {
    ev = JSON.parse(data);
  } catch {
    return;
  }
  switch (name) {
    case 'text_delta': onDelta(ev, false); break;
    case 'reasoning_delta': onDelta(ev, true); break;
    case 'tool_call': onToolCall(ev); break;
    case 'tool_result': onToolResult(ev); break;
    case 'usage': onUsage(ev); break;
    case 'task_started': onTaskStarted(ev); break;
    case 'task_finished': onTaskFinished(ev); break;
    case 'reply': onReply(ev); break;
    case 'error': onErrorEvent(ev); break;
  }
}
