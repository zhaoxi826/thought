// 通用小工具：DOM 构造、时间格式化、提示条、读屏播报。

export const $ = (sel) => document.querySelector(sel);

export function el(tag, className, text) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** 创建 type=button 的按钮（避免在 form 内误触发提交）。 */
export function button(className, text, ariaLabel) {
  const b = el('button', className, text);
  b.type = 'button';
  if (ariaLabel) b.setAttribute('aria-label', ariaLabel);
  return b;
}

export function relTime(ms) {
  const diff = Date.now() - ms;
  if (diff < 60e3) return '刚刚';
  if (diff < 3600e3) return `${Math.floor(diff / 60e3)} 分钟前`;
  if (diff < 86400e3) return `${Math.floor(diff / 3600e3)} 小时前`;
  const d = new Date(ms);
  return `${d.getMonth() + 1}月${d.getDate()}日`;
}

export function fmtElapsed(startMs, endMs = Date.now()) {
  const s = Math.max(0, Math.round((endMs - startMs) / 1000));
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m${s % 60}s`;
}

/** 右下角提示条。kind: 'error' | 'info'。错误用 role=alert 立即播报。 */
export function toast(message, kind = 'error') {
  const box = $('#toasts');
  if (!box) return;
  const t = el('div', `toast ${kind}`);
  t.setAttribute('role', kind === 'error' ? 'alert' : 'status');
  t.append(el('span', 'toast-text', message));
  const close = button('toast-close', '×', '关闭提示');
  close.addEventListener('click', () => t.remove());
  t.append(close);
  box.append(t);
  setTimeout(() => t.remove(), kind === 'error' ? 8000 : 4000);
}

/** 操作失败的统一提示；401 已由全局处理（回登录页），不重复提示。 */
export function reportError(prefix, e) {
  if (e && e.status === 401) return;
  console.error(prefix, e);
  toast(`${prefix}：${e?.message || e}`);
}

/** 给读屏播报一段文字（#sr-announce 是 aria-live=polite 区域）。 */
export function announce(text) {
  const box = $('#sr-announce');
  if (!box) return;
  box.textContent = '';
  // 下一帧再写入，保证相同文字也会被重新播报
  requestAnimationFrame(() => { box.textContent = text.length > 500 ? `${text.slice(0, 500)}…` : text; });
}
