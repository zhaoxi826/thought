// 全局可变状态（单例）。各视图模块共享同一个对象。

export const state = {
  auth: null,              // {token, expires_at, user}
  epoch: 0,                // 每次登录/登出 +1；异步回调据此丢弃上一个用户的结果
  sessions: [],
  currentId: null,
  view: 'chat',

  // SSE
  sse: null,               // 当前 SSE 连接
  sseRetry: 0,
  sseTimer: null,
  sseReconnecting: false,  // 正在重连：连上后需要重新拉历史（后端不回放事件）

  // 会话内容加载代次：每次切换/重载会话 +1，await 之后不匹配就放弃渲染
  loadGen: 0,
  resyncing: false,        // 重连后重拉历史期间，实时事件先缓冲
  pendingEvents: [],

  run: null,               // 当前运行块上下文
  toolCards: new Map(),    // call_id → 工具卡片（跨 run，按 id 全局配对）
  taskCards: new Map(),    // task_id → 任务卡片（本会话内全部，跨 run）
  runningTasks: new Map(), // task_id → 运行中的任务卡片

  memoryLoaded: false,
  memoryEnabled: true,
  memoryTab: 'search',
  systemLoaded: false,
};
