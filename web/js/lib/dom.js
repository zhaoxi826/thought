// DOM 与格式化小工具。所有动态文本一律走 textContent，杜绝 XSS。

/** 创建元素：h('div', {class:'x', title:'…', onclick:fn}, child…)。 */
export function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v == null) continue;
    if (k === 'class') el.className = v;
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k.startsWith('on') && typeof v === 'function') el.addEventListener(k.slice(2), v);
    else if (k === 'text') el.textContent = v;
    else if (k === 'html') el.innerHTML = v; // 仅限已消毒的 markdown 输出
    else el.setAttribute(k, v);
  }
  for (const c of children) {
    if (c == null) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}

/** HH:MM */
export function fmtClock(ms) {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** HH:MM:SS */
export function fmtClockS(ms) {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** 相对时间：刚刚 / n 分钟前 / n 小时前 / n 天前 */
export function fmtRel(ms) {
  const diff = Date.now() - ms;
  if (diff < 45_000) return '刚刚';
  const min = Math.floor(diff / 60_000);
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  const day = Math.floor(hr / 24);
  return `${day} 天前`;
}

/** 时长：18s / 2m10s / 1h03m */
export function fmtDur(ms) {
  if (ms < 0) ms = 0;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${pad(s % 60)}s`;
  const hr = Math.floor(m / 60);
  return `${hr}h${pad(m % 60)}m`;
}

/** 千分位 */
export function fmtNum(n) {
  return Number(n || 0).toLocaleString('en-US');
}

function pad(n) {
  return String(n).padStart(2, '0');
}
