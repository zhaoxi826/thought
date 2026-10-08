// 极简 SSE 客户端：fetch + ReadableStream（EventSource 不能带 Authorization 头）。
// 按 SSE 规范解析 event:/data: 行，多行 data 以 \n 拼接，注释行（: 开头）忽略。

/**
 * @param {string} url
 * @param {string} token
 * @param {object} cb
 *   onEvent(name, data)  收到一条事件（data 为原始字符串）
 *   onOpen()             连接建立（2xx）
 *   onClose(status)      连接断开。status 为 HTTP 状态码（如 401），0=流正常结束，-1=网络错误
 * @returns {{close(): void}}
 */
export function connectSSE(url, token, { onEvent, onOpen, onClose }) {
  const ctrl = new AbortController();
  let closed = false;

  (async () => {
    let status = -1;
    try {
      const resp = await fetch(url, {
        headers: { Authorization: `Bearer ${token}` },
        signal: ctrl.signal,
      });
      status = resp.status;
      if (!resp.ok || !resp.body) return;

      onOpen();
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = '';
      for (;;) {
        const { done, value } = await reader.read();
        if (done) { status = 0; break; }
        buf += decoder.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf('\n\n')) >= 0) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          parseFrame(frame, onEvent);
        }
      }
    } catch (e) {
      if (closed || (e && e.name === 'AbortError')) return;
    } finally {
      if (!closed) onClose(status);
    }
  })();

  return {
    close() {
      closed = true;
      ctrl.abort();
    },
  };
}

function parseFrame(frame, onEvent) {
  let name = 'message';
  const dataLines = [];
  for (const line of frame.split('\n')) {
    if (!line || line.startsWith(':')) continue;
    const colon = line.indexOf(':');
    const field = colon < 0 ? line : line.slice(0, colon);
    // 规范：冒号后一个空格要剥掉
    let value = colon < 0 ? '' : line.slice(colon + 1);
    if (value.startsWith(' ')) value = value.slice(1);
    if (field === 'event') name = value;
    else if (field === 'data') dataLines.push(value);
  }
  if (dataLines.length > 0) onEvent(name, dataLines.join('\n'));
}
