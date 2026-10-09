# thought 后端接口契约（待实现部分）

> 面向后端接手人。本文档描述前端已经就位、但后端尚缺失的两组 API：**任务注册表**与**配置读写**。
> 前端针对缺失部分做了适配层 + 优雅降级，后端落地本文档后，前端只需改适配层内部实现，UI 不动。
>
> 约定沿用现有 API：统一错误形状 `{"error": "消息"}`；除 `POST /api/auth/login` 外全部需要
> `Authorization: Bearer <token>`；401 表示登录失效（前端全局回登录页）；时间戳一律 Unix 毫秒（`*_at_ms`）。

---

## 1. 任务注册表（Task Registry）

### 1.1 背景与前端现状

> **前端已实现（按本节契约）**：`web/js/tasks-store.js` 已切换到注册表——初始 `GET /api/tasks`
> （第一页 + `status=running` 补齐长任务，列表底栏「加载更多」走 `cursor`）、全局 `GET /api/tasks/events`
> 一条 SSE（断线重连 / `lagged` 后按注册表对账）、选中任务调 1.3 详情回填活动流并与实时事件去重合并
> （工具按 `(agent, id)`，文本按该 agent 已落库文本的子串）、`POST …/cancel` 接到取消按钮。
> 逐会话 SSE 仅保留给对话页当前会话（承载 `reply`/`error`），不再参与任务聚合。下文为改造前的记录。

改造前现有 API 只有 `GET /api/sessions/{id}/tasks`（仅运行中）和逐会话 SSE
`GET /api/sessions/{id}/events`（无回放）。前端适配层 `web/js/tasks-store.js` 目前的权宜做法：

- 启动时 `GET /api/sessions`，对**每个**会话 `GET /api/sessions/{id}/tasks` 收集运行中任务；
- 对**每个**会话各开一条 SSE，按 `task_started` / `task_finished` / `tool_call` / `tool_result` /
  `text_delta` / `usage` 聚合任务对象（agent 标签形如 `coder#3`、`coder#3/sub1`）；
- 因此**任务历史只在页面存活期内有效**（UI 已如实标注「历史任务暂存于本次会话 · 持久化待后端任务注册表」）。

现有做法的问题，也是注册表要解决的：

1. `GET /api/sessions/{id}/tasks` 与 SSE 订阅都会**唤醒会话的 agent 循环**（`state.session()` 副作用），
   仅为看任务列表就把所有会话加载进内存，不可扩展；
2. 页面刷新后所有已结束任务丢失；
3. N 条会话 = N 条 SSE 长连接；
4. 无法取消任务（UI 按钮目前禁用，title「待后端支持」）。

### 1.2 `GET /api/tasks` — 全局任务列表（含历史）

```
GET /api/tasks?status=&limit=&cursor=
Authorization: Bearer <token>
```

| 参数 | 说明 |
| --- | --- |
| `status` | 可选。`running` / `done` / `error` / `cancelled`，逗号分隔可多选；缺省返回全部 |
| `limit` | 可选，默认 50，上限 200 |
| `cursor` | 可选。上一页响应里的 `next_cursor`，做游标分页（按开始时间倒序） |

响应 `200`：

```json
{
  "tasks": [
    {
      "task_id": 5,
      "session_id": "253abab4d9654eafa2c499e594ebca2e",
      "worker": "coder",
      "task": "把 fusion.rs 的截断基准改成：首条为离群值时退回中位数，并补回归测试。",
      "status": "running",
      "started_at_ms": 1791474802000,
      "finished_at_ms": null,
      "result": null,
      "error": null,
      "usage": { "prompt_tokens": 1200, "completion_tokens": 672, "total_tokens": 1872 }
    }
  ],
  "next_cursor": "eyJ0IjoxNzkxNDc0MDAwMDAwLCJpIjoiYWJjIn0"
}
```

- `task_id` 在**会话内**唯一（与现有 `task_started` 事件一致）；全局主键请视为 `(session_id, task_id)`，
  前端任务 key 就是 `${session_id}:${task_id}`。
- `status`：`running` / `done` / `error` / `cancelled`（取消归入 `cancelled`，与现有 `task_finished`
  的 `error="cancelled"` 约定一致）。
- `usage` 为该任务各次模型调用用量之和；运行中任务实时累计。
- `next_cursor` 为 `null` 表示没有更多。
- 该接口**不得**有唤醒会话循环的副作用；数据源应是持久化存储（SurrealKV 一张 task 表）。

错误：沿用 `{"error": ...}`；非法 cursor → `400 {"error": "invalid cursor"}`。

### 1.3 `GET /api/tasks/{session_id}/{task_id}` — 任务详情（含活动事件）

前端任务页详情区需要活动流（活动流 UI 已就绪，目前只能展示页面存活期内 SSE 收到的事件）。

```
GET /api/tasks/{session_id}/{task_id}
```

响应 `200`：在 1.2 的单任务字段基础上增加 `events`：

```json
{
  "task_id": 5,
  "session_id": "253abab4d9654eafa2c499e594ebca2e",
  "worker": "coder",
  "task": "…",
  "status": "done",
  "started_at_ms": 1791474802000,
  "finished_at_ms": 1791474849000,
  "result": "截断逻辑确认有误……",
  "error": null,
  "usage": { "prompt_tokens": 3000, "completion_tokens": 1213, "total_tokens": 4213 },
  "events": [
    { "ts_ms": 1791474803000, "kind": "tool_call",   "agent": "coder#5",      "id": "call_1", "name": "read_file", "arguments": "{\"file_path\":\"symbol/src/fusion.rs\"}" },
    { "ts_ms": 1791474803300, "kind": "tool_result", "agent": "coder#5",      "id": "call_1", "name": "read_file", "output": "…" },
    { "ts_ms": 1791474811000, "kind": "text",        "agent": "coder#5",      "text": "确认问题点……" },
    { "ts_ms": 1791474812000, "kind": "tool_call",   "agent": "coder#5/sub1", "id": "call_2", "name": "read_file", "arguments": "…" },
    { "ts_ms": 1791474820000, "kind": "usage",       "agent": "coder#5",      "usage": { "prompt_tokens": 1200, "completion_tokens": 672, "total_tokens": 1872 } }
  ]
}
```

建议：持久化现有 `AgentEvent` 的一个子集即可——`text`（`text_delta` 按 agent 连续段合并存储）、
`tool_call`、`tool_result`、`usage`，都带上 `agent` 标签与落库时间戳。`reasoning_delta` 不需要。
子 agent 通过 `agent` 标签里的 `/subN` 后缀表达（前端按此前缀嵌套展示）。
找不到 → `404 {"error": "task not found"}`。

### 1.4 `POST /api/tasks/{session_id}/{task_id}/cancel` — 取消任务

```
POST /api/tasks/{session_id}/{task_id}/cancel
```

- `204`：取消请求已接受。执行效果与主控 `cancel_task` 工具一致：随后该任务以
  `task_finished { error: "cancelled" }` 结束。
- `404 {"error": "task not found"}`；`409 {"error": "task already finished"}`。

前端落地位置：`web/js/tasks.js` 详情头部的「取消」按钮（当前 `disabled`，title「待后端支持」）。

### 1.5 `GET /api/tasks/events` — 全局任务事件流（SSE）

```
GET /api/tasks/events
Accept: text/event-stream
```

聚合**所有会话**的任务相关事件，事件名与现有逐会话 SSE 一致，data 里多一个 `session_id` 字段：

```
event: task_started
data: {"session_id":"…","task_id":5,"worker":"coder","task":"…"}

event: tool_call
data: {"session_id":"…","agent":"coder#5","id":"call_1","name":"read_file","arguments":"…"}

event: task_finished
data: {"session_id":"…","task_id":5,"worker":"coder","result":"…","error":null}
```

需要的事件子集：`task_started` / `task_finished` / `tool_call` / `tool_result` / `text_delta` / `usage`。
慢消费者丢失用 `event: lagged` + 纯数字 data（沿用现有约定）。
同样**不得**唤醒会话循环——从任务注册表的写路径上扇出，而不是从各会话的 broadcast 上桥接。

前端落地位置：`web/js/tasks-store.js` 的 `ensureStream()`——把「逐会话各开一条 SSE」换成这一条全局流，
`handleEvent()` 及之后的聚合逻辑不用改（`session_id` 字段正好填进现在从连接上下文取的值）。

### 1.6 前端换数据源时要改的点（汇总）

全部在 `web/js/tasks-store.js`（文件头注释也写了这三处）：

| 位置 | 现在 | 换成 |
| --- | --- | --- |
| `start()` 里的初始收集 | 逐会话 `GET /api/sessions/{id}/tasks` | `GET /api/tasks`（含历史、分页） |
| `ensureStream(id)` | 逐会话 SSE ×N | 全局 `GET /api/tasks/events` ×1 |
| （新增）取消按钮 | 禁用 | `POST /api/tasks/{session_id}/{task_id}/cancel` |

任务详情历史回填：选中任务时调 1.3 的详情接口补齐 `events`（`web/js/tasks.js` `renderDetail()` 入口）。

---

## 2. 配置读写

### 2.1 背景与前端现状

> **前端已实现（按本节契约）**：`web/js/settings.js`（+ `settings-form.js`）用 `GET /api/config` 填充全部分区，
> admin 可编辑，「保存」→ `PUT /api/config`（`If-Match` 回传 ETag，body 为 GET 响应原样 + 修改）；
> 412 提示并可重新加载，422 显示 `error`，200 按 `restart_required` / `hot_applied` 分行提示（热生效注明仅影响新会话）。
> 「原始 TOML」读 `?format=toml`（只读）。404 时退回 `/api/models` + `/api/agents` 只读；非 admin 保持只读/受限。
> 已有密钥的 `*_env` 变量名锁定不可改，仅新增模型可填 `api_key_env`。任务页并发仪表 N 读 `GET /api/agents` 的
> `limits.max_concurrent_tasks`（按会话计，分子取进行中最多的会话）。下文为改造前的记录。

改造前设置页（`web/js/settings.js` + `web/index.html` 骨架）：

- **模型池卡**：`GET /api/models`（admin；403 显示「仅管理员可见」），只读禁用态呈现；
- **Agent 卡**：`GET /api/agents`（controller.model + workers 的 description/model/tools/can_spawn），只读；
- **服务 / 认证 / 存储 / 记忆 / 限额**：无数据源，骨架占位「—」，卡片标注「配置 API 待后端实现」；
- 顶部「保存」禁用；「原始 TOML」视图显示「待配置 API 上线后可用」。

注意 `/api/agents` 目前**不含** worker 的 `system` 提示词，也不含 `[agent.limits]`；
设置页相应字段已留位（limits 三栏），配置 API 上线后请一并在响应里带出。

### 2.2 `GET /api/config` — 读取配置（admin）

推荐结构化 JSON（与 `ServerConfig` 同形，即 `thought.toml` 解析后的形状），并带 `ETag` 供条件请求与写回冲突检测：

```
GET /api/config
Authorization: Bearer <admin token>
```

响应 `200`：

```
ETag: "b3a9f1c2"
```

```json
{
  "http": { "bind": "127.0.0.1:8080", "web_dir": "web" },
  "auth": {
    "jwt_secret_env": "THOUGHT_JWT_SECRET",
    "token_ttl_secs": 43200,
    "bootstrap_admin": { "username": "admin", "password_env": "THOUGHT_ADMIN_PASSWORD" }
  },
  "storage": { "endpoint": "surrealkv://data/thought", "namespace": "thought", "database": "server" },
  "models": {
    "deepseek": { "provider": "deepseek", "model": "deepseek-v4-pro", "api_key_env": "DEEPSEEK_API_KEY" }
  },
  "embedding": null,
  "memory": {
    "namespace": "thought", "database": "memory", "embedding_dimension": 1024,
    "extractor_model": "deepseek-flash", "per_user": true, "save_user_messages": false,
    "half_life_hours": 168.0
  },
  "agent": {
    "controller": { "model": "deepseek", "tools": [] },
    "workers": [
      { "name": "coder", "description": "…", "model": "deepseek", "system": "…",
        "tools": ["read_file", "write_file", "delete_file", "run_command"], "can_spawn": true }
    ],
    "limits": { "max_concurrent_tasks": 4, "subagent_max_turns": 30, "max_tool_output_chars": 32768 }
  }
}
```

安全要求（与现有 `/api/models` 一致）：**密钥一律不输出**。`api_key` / `jwt_secret` / `password`
等明文字段要么剔除、要么只回 `*_env` 变量名；响应里宁可缺字段也不能泄密。
非 admin → `403 {"error": "admin only"}`。

「原始 TOML」视图如果也要支持，可加 `GET /api/config?format=toml` 返回 `text/plain` 原文；
优先级低于结构化 JSON。

### 2.3 `PUT /api/config` — 写回配置（admin）

```
PUT /api/config
Authorization: Bearer <admin token>
Content-Type: application/json
If-Match: "b3a9f1c2"

{ 同 GET /api/config 的结构化 JSON（全量） }
```

- 服务端做 schema 校验（复用 `ServerConfig` 的 Deserialize + 语义校验，如 bind 可解析、模型引用存在），
  校验通过写回 `thought.toml`（建议先写临时文件再原子替换，保留一份 `.bak`）。
- `If-Match` 与当前 ETag 不符 → `412 {"error": "config changed, reload first"}`；
  校验失败 → `422 {"error": "具体校验信息"}`。

响应 `200`：标注**哪些节需要重启生效**，前端据此提示用户：

```json
{
  "applied": true,
  "restart_required": ["http", "storage", "auth"],
  "hot_applied": ["agent.limits", "agent.workers", "models"]
}
```

建议的热生效划分（按现有启动路径判断，实现时以后端实际为准）：

| 节 | 生效方式 |
| --- | --- |
| `http`（bind / web_dir） | **需重启**（监听 socket 与静态托管在启动时绑定） |
| `storage`（endpoint / namespace / database） | **需重启**（内嵌 SurrealKV 启动时打开） |
| `auth`（jwt_secret_env / token_ttl / bootstrap_admin） | **需重启**（JWT 签发器启动时构建；且 secret 变了旧 token 全部失效） |
| `models.*` | 建议**可热生效**（agent 系统按名查模型池，重建或替换池内条目即可） |
| `agent.limits` | 建议**可热生效**（并发/轮数/输出上限是运行期读取的值） |
| `agent.workers` / `agent.controller` | 建议**可热生效**（影响新会话/新任务；进行中的会话可不动） |
| `memory` / `embedding` | 折中：记忆维护 worker 可重建；至少 `per_user`、`save_user_messages` 可热生效 |

### 2.4 前端换数据源时要改的点（汇总）

- `web/js/settings.js`：骨架分区（服务/认证/存储/记忆/限额）改为 `GET /api/config` 填充并放开编辑；
  「保存」按钮接 `PUT /api/config`（带 `If-Match`），按 `restart_required` 提示；
  「原始 TOML」视图接 `?format=toml`。
- `web/js/tasks.js` 状态带并发仪表：`agent.limits.max_concurrent_tasks` 有了之后补全「N」
  （代码里标了 `TODO(backend)`，现在只显示进行中数）。

---

## 3. 其他已对齐的现状（无需改动，供参考）

- 401：除 login 外任何接口 401，前端清登录态回登录页；SSE 重连遇 401 同样处理。
- SSE 断线：前端指数退避重连（1s→30s）；`lagged` 事件仅提示，无回放可接受。
- 记忆未启用：`/api/memory*` 返回 `404 "memory is not enabled"`，前端显示「记忆未启用」。
- `/api/models` 仅 admin：403 时模型池卡显示「仅管理员可见」。
