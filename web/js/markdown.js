// 极简 markdown 渲染。防 XSS 是硬要求：输入先整体 HTML 转义，之后所有变换
// 只在转义后的文本上进行；链接 URL 做协议白名单。输出为安全 HTML 字符串。

const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

// 占位符：转义后的文本里不会出现该控制字符
const PH = '\u0001';

export function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

/** 渲染 markdown 文本为安全 HTML。 */
export function renderMarkdown(src) {
  const escaped = escapeHtml(src ?? '');
  const segments = splitCodeFences(escaped);
  let html = '';
  for (const seg of segments) {
    html += seg.type === 'code' ? codeBlockHtml(seg.lang, seg.code) : renderTextBlocks(seg.text);
  }
  return html;
}

// ---- 代码块（``` 围栏，容忍未闭合） ----

function splitCodeFences(escaped) {
  const segments = [];
  let rest = escaped;
  const fenceRe = /```([^\n`]*)\n?/;
  for (;;) {
    const m = fenceRe.exec(rest);
    if (!m) {
      if (rest) segments.push({ type: 'text', text: rest });
      break;
    }
    if (m.index > 0) segments.push({ type: 'text', text: rest.slice(0, m.index) });
    rest = rest.slice(m.index + m[0].length);
    const end = rest.indexOf('```');
    if (end < 0) {
      // 未闭合：其余全部当代码（流式渲染时常见）
      segments.push({ type: 'code', lang: m[1].trim(), code: rest });
      rest = '';
      break;
    }
    segments.push({ type: 'code', lang: m[1].trim(), code: rest.slice(0, end).replace(/\n$/, '') });
    rest = rest.slice(end + 3);
    if (rest.startsWith('\n')) rest = rest.slice(1);
  }
  return segments;
}

function codeBlockHtml(lang, code) {
  const langLabel = lang || 'text';
  return (
    `<pre class="codeblock"><div class="codeblock-head">` +
    `<span class="codeblock-lang">${escapeHtml(langLabel)}</span>` +
    `<button type="button" class="copy-btn" data-copy>复制</button>` +
    `</div><code>${code}</code></pre>`
  );
}

// ---- 块级：标题 / 列表 / 分隔线 / 普通段落 ----

function renderTextBlocks(text) {
  const lines = text.split('\n');
  let html = '';
  let para = [];
  let list = null; // {tag: 'ul'|'ol', items: []}

  const flushPara = () => {
    if (para.length) {
      html += `<p>${para.map(inline).join('<br>')}</p>`;
      para = [];
    }
  };
  const flushList = () => {
    if (list) {
      html += `<${list.tag}>${list.items.map((i) => `<li>${inline(i)}</li>`).join('')}</${list.tag}>`;
      list = null;
    }
  };

  for (const line of lines) {
    const trimmed = line.trim();
    let m;
    if ((m = /^(#{1,4})\s+(.*)$/.exec(trimmed))) {
      flushPara(); flushList();
      const level = m[1].length;
      html += `<h${level}>${inline(m[2])}</h${level}>`;
    } else if ((m = /^[-*]\s+(.*)$/.exec(trimmed))) {
      flushPara();
      if (!list || list.tag !== 'ul') { flushList(); list = { tag: 'ul', items: [] }; }
      list.items.push(m[1]);
    } else if ((m = /^\d+\.\s+(.*)$/.exec(trimmed))) {
      flushPara();
      if (!list || list.tag !== 'ol') { flushList(); list = { tag: 'ol', items: [] }; }
      list.items.push(m[1]);
    } else if (/^(-{3,}|\*{3,})$/.test(trimmed)) {
      flushPara(); flushList();
      html += '<hr>';
    } else if (trimmed === '') {
      flushPara(); flushList();
    } else {
      flushList();
      para.push(trimmed);
    }
  }
  flushPara(); flushList();
  return html;
}

// ---- 行内：代码、链接、粗体、斜体 ----

function inline(text) {
  // 行内代码先占位保护，避免内部内容被后续规则处理
  const codes = [];
  let s = text.replace(/`([^`\n]+)`/g, (_, code) => {
    codes.push(code);
    return `${PH}${codes.length - 1}${PH}`;
  });

  // 链接 [文字](URL)：URL 只允许 http(s)/mailto/站内路径，其余降级为纯文字
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, url) => {
    const safe = /^((https?:|mailto:)[^\s]*|\/[^\s]*|#[^\s]*)$/i.test(url);
    if (!safe) return label;
    const href = url.replace(/&quot;/g, '%22');
    return `<a href="${href}" target="_blank" rel="noopener noreferrer">${label}</a>`;
  });

  s = s
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/__([^_]+)__/g, '<strong>$1</strong>')
    .replace(/\*([^*\n]+)\*/g, '<em>$1</em>')
    .replace(/(^|[^a-zA-Z0-9])_([^_\n]+)_/g, '$1<em>$2</em>');

  // 还原行内代码
  const phRe = new RegExp(`${PH}(\\d+)${PH}`, 'g');
  s = s.replace(phRe, (_, i) => `<code class="inline">${codes[+i]}</code>`);
  return s;
}
