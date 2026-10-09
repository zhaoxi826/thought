// fetch 封装：token 注入、401 全局处理、统一错误格式 {"error": "..."}
//
// 登录态存 localStorage（键 thought.auth）：{ token, expires_at(Unix 秒), user:{id,username,role} }。
// 启动时先 GET /api/me 验证，过期或 401 即清除并回登录页。

const AUTH_KEY = 'thought.auth';

export class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

export function loadAuth() {
  try {
    const raw = localStorage.getItem(AUTH_KEY);
    if (!raw) return null;
    const auth = JSON.parse(raw);
    // 过期即视为未登录
    if (!auth.token || (auth.expires_at && auth.expires_at * 1000 <= Date.now())) {
      localStorage.removeItem(AUTH_KEY);
      return null;
    }
    return auth;
  } catch {
    return null;
  }
}

export function saveAuth(auth) {
  localStorage.setItem(AUTH_KEY, JSON.stringify(auth));
}

export function clearAuth() {
  try {
    localStorage.removeItem(AUTH_KEY);
  } catch { /* 忽略 */ }
}

let onUnauthorized = () => {};
export function setUnauthorizedHandler(fn) {
  onUnauthorized = fn;
}

/**
 * 请求 API。成功返回解析后的 JSON（204 返回 null）。
 * 401 时清除登录态并触发全局回登录页（skip401 可跳过，登录接口用）。
 */
export async function api(path, opts = {}) {
  const resp = await apiRaw(path, opts);
  if (resp.status === 204) return null;
  const text = await resp.text();
  return text ? JSON.parse(text) : null;
}

/**
 * 底层请求：错误处理同 api()，成功时返回原始 Response（需要读响应头 / 纯文本时用）。
 * headers 为额外请求头（如 If-Match）。
 */
export async function apiRaw(path, { method = 'GET', body, headers: extra = {}, skip401 = false } = {}) {
  const auth = loadAuth();
  const headers = { ...extra };
  if (auth) headers['Authorization'] = `Bearer ${auth.token}`;
  if (body !== undefined) headers['Content-Type'] = 'application/json';

  const resp = await fetch(path, {
    method,
    headers,
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });

  if (resp.status === 401 && !skip401) {
    clearAuth();
    onUnauthorized();
    throw new ApiError(401, '登录已过期');
  }
  if (!resp.ok) {
    let message = `HTTP ${resp.status}`;
    try {
      const j = await resp.json();
      if (j && j.error) message = j.error;
    } catch { /* 非 JSON 响应 */ }
    throw new ApiError(resp.status, message);
  }
  return resp;
}
