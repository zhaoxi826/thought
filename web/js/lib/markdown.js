// 极简 markdown 渲染。防 XSS 是硬要求：输入先整体 HTML 转义，之后所有变换
// 只在转义后的文本上进行；链接 URL 做协议白名单。输出为安全 HTML 字符串。

const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

// 占位符分隔符：渲染前会从输入里剔除，因此不会与用户文本冲突
const PH = '\u0001';
const CODE_PH_RE = new RegExp(`${PH}c(\\d+)${PH}`, 'g');
const LINK_PH_RE = new RegExp(`${PH}l(\\d+)${PH}`, 'g');

// 允许的链接：http(s)、mailto、站内绝对路径（拒绝 //host 与 /\host 这类协议相对 URL）
const SAFE_URL_RE = /^(?:https?:|mailto:|\/(?![/\\]))/i;

export function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

/** 渲染 markdown 文本为安全 HTML。 */
export function renderMarkdown(src) {
  const escaped = escapeHtml(String(src ?? '').replaceAll(PH, ''));
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
  // lang 取自已整体转义过的文本，不能再转义一次（否则 c"x 会显示成 c&quot;x）
  const langLabel = lang || 'text';
  return (
    `<pre class="codeblock"><div class="codeblock-head">` +
    `<span class="codeblock-lang">${langLabel}</span>` +
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

function emphasis(s) {
  return s
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/__([^_]+)__/g, '<strong>$1</strong>')
    .replace(/\*([^*\n]+)\*/g, '<em>$1</em>')
    .replace(/(^|[^a-zA-Z0-9])_([^_\n]+)_/g, '$1<em>$2</em>');
}

function inline(text) {
  // 1. 行内代码先占位保护，避免内部内容被后续规则处理
  const codes = [];
  let s = text.replace(/`([^`\n]+)`/g, (_, code) => {
    codes.push(code);
    return `${PH}c${codes.length - 1}${PH}`;
  });

  // 2. 链接 [文字](URL) 整体换成占位符，href 不再经过任何后续正则。
  //    URL 不在白名单内时降级为纯文字。
  const links = [];
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, rawUrl) => {
    // URL 里若被行内代码规则吃掉了反引号，还原成原文
    const url = rawUrl.replace(CODE_PH_RE, (_m, i) => `\`${codes[+i]}\``);
    if (!SAFE_URL_RE.test(url)) return label;
    // url 已是 HTML 转义后的文本，放进双引号属性里是安全的；引号再转成 %22 更稳妥
    const href = url.replace(/&quot;/g, '%22');
    links.push(`<a href="${href}" target="_blank" rel="noopener noreferrer">${emphasis(label)}</a>`);
    return `${PH}l${links.length - 1}${PH}`;
  });

  // 3. 粗体 / 斜体
  s = emphasis(s);

  // 4. 先还原链接（标签里可能含代码占位符），再还原行内代码
  s = s.replace(LINK_PH_RE, (_, i) => links[+i]);
  s = s.replace(CODE_PH_RE, (_, i) => `<code class="inline">${codes[+i]}</code>`);
  return s;
}
