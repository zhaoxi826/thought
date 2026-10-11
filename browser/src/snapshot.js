// 页面快照：给可见的可交互元素打上编号（data-thought-ref），输出可读的页面结构。
// 返回 { url, title, text }。编号在页面变化后可能失效，重新快照即可。
(() => {
  const MAX = __MAX__;
  const interactive = 'a[href],button,input,textarea,select,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[role=combobox],[contenteditable=""],[contenteditable=true],[onclick],[tabindex]:not([tabindex="-1"])';
  document.querySelectorAll('[data-thought-ref]').forEach(e => e.removeAttribute('data-thought-ref'));
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return false;
    const s = getComputedStyle(el);
    return s.visibility !== 'hidden' && s.display !== 'none' && s.opacity !== '0';
  };
  const clean = (t) => (t || '').replace(/\s+/g, ' ').trim();
  const label = (el) => clean(
    el.getAttribute('aria-label') || el.getAttribute('title') || el.getAttribute('placeholder') ||
    (el.labels && el.labels[0] && el.labels[0].innerText) || el.innerText || el.value || el.getAttribute('alt') || el.getAttribute('name') || ''
  ).slice(0, 120);
  let n = 0;
  const lines = [];
  const walk = (node, depth) => {
    if (lines.join('\n').length > MAX) return;
    if (node.nodeType === Node.TEXT_NODE) {
      const t = clean(node.textContent);
      if (t && node.parentElement && !node.parentElement.closest('[data-thought-ref]') && visible(node.parentElement)) {
        lines.push('  '.repeat(depth) + t.slice(0, 300));
      }
      return;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const el = node;
    const tag = el.tagName.toLowerCase();
    if (['script', 'style', 'noscript', 'template', 'svg'].includes(tag)) return;
    if (!visible(el) && tag !== 'body') return;
    if (el.matches(interactive)) {
      n += 1;
      el.setAttribute('data-thought-ref', String(n));
      const role = el.getAttribute('role') || (tag === 'input' ? `input:${el.type || 'text'}` : tag);
      let extra = '';
      if (tag === 'a') extra = ` → ${el.getAttribute('href')}`;
      if (tag === 'input' || tag === 'textarea') extra = el.type === 'password' ? ' value=••••' : (el.value ? ` value=${JSON.stringify(el.value.slice(0, 80))}` : '');
      if (tag === 'select') extra = ` value=${JSON.stringify(el.value)}`;
      if (el.type === 'checkbox' || el.type === 'radio') extra = el.checked ? ' [x]' : ' [ ]';
      if (el.disabled) extra += ' (disabled)';
      lines.push(`${'  '.repeat(depth)}[${n}] ${role} ${JSON.stringify(label(el))}${extra}`);
      return;
    }
    if (/^h[1-6]$/.test(tag)) {
      lines.push(`${'  '.repeat(depth)}${'#'.repeat(Number(tag[1]))} ${clean(el.innerText).slice(0, 200)}`);
      return;
    }
    if (tag === 'img' && el.alt) lines.push(`${'  '.repeat(depth)}[img] ${clean(el.alt).slice(0, 120)}`);
    for (const c of el.childNodes) walk(c, depth + (['ul', 'ol', 'form', 'nav', 'table', 'dialog'].includes(tag) ? 1 : 0));
    if (el.shadowRoot) for (const c of el.shadowRoot.childNodes) walk(c, depth + 1);
  };
  walk(document.body || document.documentElement, 0);
  let text = lines.join('\n');
  if (text.length > MAX) text = text.slice(0, MAX) + '\n…[truncated]';
  const scroll = `scroll ${Math.round(scrollY)}/${Math.max(0, document.documentElement.scrollHeight - innerHeight)}`;
  return { url: location.href, title: document.title, text, scroll, refs: n };
})()
