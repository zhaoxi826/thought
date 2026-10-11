# thought

多 agent 系统：队列驱动的三层 agent + SurrealDB 持久化记忆 + JWT 认证的 HTTP 后端。

## crate

| crate | 作用 |
| --- | --- |
| `model` | 大模型接口层：OpenAI / DeepSeek / Anthropic 协议、流式、工具调用消息、embedding、`ModelConfig`。见 [model/README.md](model/README.md) |
| `plugin` | `Tool` trait + `Registry`，内置 `read_file` / `write_file` / `delete_file` / `run_command` |
| `agent` | 三层 agent 系统与共用的 ReAct 执行器 `Runner` |
| `symbol` | 持久化记忆器 `MemoryStore`（参考 [A_memorix](https://github.com/A-Dawn/A_memorix)），记忆工具 |
| `browser` | 她专属的 Chromium（CDP 完全控制）：`browser_*` 工具——导航、快照编号定位、真实鼠标键盘、JS、截图、cookie、上传下载、多标签，`browser_cdp` 发任意 CDP 命令 |
| `server` | axum 后端：登录 / JWT、会话 + SSE、记忆接口；`server::build` 组装一切 |
| `thought-core` | 二进制 `thought`：读配置、注册工具、启动服务 |

## agent 设计

```
          server 消息 ─┐
   到期待办(add_todo) ─┼─▶ 队列 ─▶ L1 主控 ──dispatch_task──▶ L2 worker（配置定义，独立 task）
     任务结果 ◀────────┘                                        │ spawn_subagent
                                                                 ▼
                                                            L3 子 agent（继承工具，不能再派生）
```

- **L1 主控**：每个会话一个，只持有元工具 `dispatch_task` / `list_tasks` / `cancel_task` / `add_todo`
  （外加会话的记忆工具），不直接干活。一次取出队列里积压的所有输入合并处理；回复 `NO_REPLY`
  时不打扰用户。
- **L2 worker**：在 `[[agent.workers]]` 里配置 name / description / model / system / tools。
  异步运行，结果以 `<task_result>` 回到主控队列。并发数受 `max_concurrent_tasks` 限制。
- **L3 子 agent**：L2 同步调用 `spawn_subagent`，同一轮多个调用并发执行。
- 所有层流式调用模型，事件（文本碎片、工具调用 / 结果、任务起止、用量、回复）广播给订阅者。

## 记忆设计（symbol）

- 数据：`paragraph`（内容 hash 去重、`external_id` 幂等）、`entity`、`relation`
  （`entity->relation->entity` 图边，记录支撑段落）。
- 写入：段落和向量同步落库；LLM 三元组抽取排队，由后台 `process_pending` 处理。
- 检索：三路融合
  - 段落向量 KNN
  - 段落 BM25（Rust 侧对中文切二元组）
  - 关系向量 / BM25 命中后按强度加权，再展开到支撑段落
  - 融合后按 `max(min_score, 首条 × relative_cutoff)` 截断
- 生命周期：关系强度按半衰期惰性衰减。检索命中时做访问强化（有冷却期）。低于阈值先冻结，更低时删除；钉住的关系不衰减。
- 隔离：每个用户一个 SurrealDB database（`per_user = true`）。

## 运行

```bash
cp thought.example.toml thought.toml       # 按需修改
export THOUGHT_JWT_SECRET=$(openssl rand -hex 32)
export THOUGHT_ADMIN_PASSWORD='...'        # 首次启动创建 admin
export DEEPSEEK_API_KEY=sk-...
cargo run --release -p thought-core -- thought.toml
```

前端：`web/` 是纯静态 SPA（原生 HTML/CSS/ES modules，无构建步骤），由服务器同源托管在 `/`。
三个页面经左侧 rail 幕帘切换：对话（浅色书斋）、任务（深色科技风，任务数据走
`js/tasks-store.js` 适配层，持久化待后端任务注册表）、设置（模型池/Agent 只读真实数据，
其余分区骨架占位待配置 API，契约见 `design/backend-api.md`）。
`http.web_dir` 默认 `web`；配置里的相对路径（`web_dir`、`surrealkv://` 存储路径）都相对
配置文件所在目录解析。浏览器打开监听地址即可登录使用。

打包：

```bash
cargo build --profile dist -p thought-core    # strip + thin LTO，产物 target/dist/thought
```

发布物 = `thought` 二进制（约 110MB，gzip 后约 40MB，主要是内嵌的 SurrealDB）+ `web/`
（约 140KB）+ `thought.toml`。

## 目录结构

```
model/src/    core/（消息、输出、流式、工具定义、错误） providers/（openai、anthropic、embedding、config、sse）
agent/src/    core/（配置、事件、Runner） orchestration/（会话队列、元工具）
symbol/src/   memory/（store、retrieve、extract、embed、text、tools、types）
server/src/   core/（auth、db、error、state） routes/（auth、admin、sessions、memory、web）
web/          css/{base,home,tasks,settings}  js/{lib,main,home,tasks-store,tasks,settings}
```

## HTTP 接口

除 `GET /health` 和 `POST /api/auth/login`（`{username, password}` → `{token, expires_at, user}`）外，
都需要带 `Authorization: Bearer <token>`。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/me` | 当前用户 |
| GET / POST | `/api/users` | 列出 / 创建用户（admin） |
| GET | `/api/models` | 模型配置（admin，不含密钥） |
| GET | `/api/agents` | 主控与 worker 定义 |
| GET / POST | `/api/sessions` | 列出 / 新建会话 |
| GET / DELETE | `/api/sessions/{id}` | 会话详情（含历史）/ 删除 |
| POST | `/api/sessions/{id}/messages` | `{content}` 投递到主控队列，返回 202 |
| GET | `/api/sessions/{id}/events` | SSE：`text_delta` `tool_call` `tool_result` `task_started` `task_finished` `usage` `reply` `error` |
| GET | `/api/sessions/{id}/tasks` | 运行中的任务 |
| GET / POST | `/api/memory` | 最近记忆 / 写入 `{content, tags?, external_id?}` |
| GET | `/api/memory/search?q=&limit=` | 检索 |
| DELETE | `/api/memory/{id}` | 删除 |

## 账号

没有注册：只能登录。账号只有两个来源——首次启动时按 `[auth.bootstrap_admin]` 创建的管理员，
以及管理员经 `POST /api/users` 创建的用户。

## 安全须知

- `run_command` / `write_file` / `delete_file` **没有沙箱**，以服务进程的权限运行。
  任何登录用户都能通过 coder worker 间接执行命令，所以只把账号发给可信的人。最好在容器或受限用户下运行。
- 默认只监听 `127.0.0.1`。对外开放请放在 TLS 反向代理之后（JWT 走明文 HTTP 会被窃取）。
- 密钥（JWT、模型 API key、管理员密码）都支持 `*_env` 从环境变量读取。`api_key` 永远不会被序列化输出。
- 登录与二次鉴权按用户名限流（连续 5 次失败锁 15 分钟）。尚无 token 吊销和改密接口。
- 她执行的命令不继承服务进程的环境变量（密钥不会泄露给她）；网页 / 文件 / 命令输出以「外部数据」围栏交给她（提示注入防护）。
- 任何登录用户都能和她说话，而她能动用浏览器与 shell。多用户的权限边界尚未细分：只把账号发给可信的人。

## 测试

```bash
cargo test --workspace     # 全部离线：mock 模型 + 内存 SurrealDB
API_KEY=sk-... cargo test -p model -- --ignored   # 联网集成测试
```
