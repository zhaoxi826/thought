# 后端内存布局数据结构与方法全景调研

> 调研范围：整个 workspace 的后端部分（纯 Rust），不含 `web/`、`design/` 前端内容。
> 调研方式：逐文件通读全部 `src/**/*.rs`（tests 目录仅经公开 API 测试、未被 src 引用，未纳入）。
> 日期：2026-10-10。

## 总览

Workspace 共 6 个 crate，分层与依赖关系：

```
thought-core (bin, 无数据结构，纯装配入口)
  └─ server   → 组装层: HTTP 路由、认证、配置存储、SurrealDB、任务注册表
       ├─ agent   → L1 主控 / L2 worker / L3 子 agent 三层系统，ReAct 执行器
       │    ├─ model  → 大模型协议适配 (OpenAI / DeepSeek / Anthropic / Embedding)
       │    └─ plugin → Tool trait + Registry + 4 个内置文件/命令工具
       └─ symbol  → 持久化记忆 (SurrealDB: 段落 + 实体/关系图 + 三路检索融合)
```

关键持有关系（谁持有谁）：

- `AppState → Arc<Inner>` 是系统中枢：持有 `Db`、`TaskRegistry`、`RwLock<AgentSystem>`、`RwLock<ServerConfig>`、`Option<MemoryStore>`、`Mutex<HashMap<String, LiveSession>>`。
- `AgentSystem → Arc<SystemInner>`（config + 模型池 `HashMap<String, Model>` + 工具池 `Registry`）；`SessionHandle → Arc<Session>`；`Session` 用 `WeakUnboundedSender` 持有自己的主控队列防止引用环。
- `TaskRegistry` 以 `Arc<dyn EventObserver>` 被 agent 事件回调；`DbSink` 以 `HistorySink` 被主控循环回调——两条回到存储的环路。
- `MemoryStore` 由 `Arc<MemoryConfig>` + `Option<Arc<dyn Embedder>>` + `Option<Arc<dyn Extractor>>` 组成，Clone 即共享连接；DB 内 `relation.paragraphs` 反向引用 `paragraph` 行。
- 工具链：`Registry(BTreeMap<String, Arc<dyn Tool>>)` 是唯一聚合点。agent 的元工具、symbol 的记忆工具、plugin 的文件工具全部经它暴露给模型。

---

# 1. `plugin` crate — 工具抽象

位置：`plugin/`，5 个 src 文件。职责：`Tool` trait 与 `Registry`，以及 4 个内置文件/命令工具。

## 1.1 数据结构

### trait `Tool`（`src/tool.rs:15`）

约束 `Send + Sync`，`#[async_trait]`，是 `dyn Tool` trait object 的基础。无字段，3 个方法：

- `fn name(&self) -> &str` — 工具名，与 definition 的 name 一致，用于分发。
- `fn definition(&self) -> Function` — 发给模型的工具定义（`Function` 来自 `model` crate）。
- `async fn call(&self, arguments: &str) -> String` — 执行工具，入参为模型回传的原始 JSON 串，返回结果文本。

### struct `Registry`（`src/tool.rs:31`）

derive `Default, Clone`（Clone 仅克隆 `Arc`，浅拷贝）；无泛型。

- `tools: BTreeMap<String, Arc<dyn Tool>>`（私有）— 按工具名有序持有工具，`BTreeMap` 保证 definitions 顺序稳定、利于 prompt 缓存。

### 内置工具（`src/tools/file_tools/`）

| 工具 | 字段 | 配对参数结构（私有，Deserialize） |
|---|---|---|
| `ReadFile`（单元 struct） | 无 | `ReadFileArgs { file_path: String }` |
| `WriteFile`（单元 struct） | 无 | `WriteFileArgs { file_path: String, file_content: String }` |
| `DeleteFile`（单元 struct） | 无 | `DeleteFileArgs { file_path: String }` |
| `RunCommand` | `pub timeout: std::time::Duration`（唯一带 pub 字段的工具） | `RunCommandArgs { command: String }` |

## 1.2 方法清单

### `impl Registry`（全部 pub）

- `new() -> Self` — 构造空注册表（走 `Default`）。
- `register(&mut self, tool: Arc<dyn Tool>) -> &mut Self` — 以 `tool.name()` 为键注册，同名覆盖，支持链式。
- `definitions(&self) -> Vec<Function>` — 收集全部工具的模型侧定义（BTreeMap 顺序稳定）。
- `call(&self, name: &str, arguments: &str) -> Option<String>` — 按名分发调用，不存在返回 `None`。
- `contains(&self, name: &str) -> bool` — 是否已注册。
- `names(&self) -> impl Iterator<Item = &str>` — 已注册工具名（有序）。
- `len(&self) -> usize` / `is_empty(&self) -> bool` — 数量。
- `extend(&mut self, other: &Registry) -> &mut Self` — 并入另一个注册表（`Arc::clone` 浅拷贝，同名覆盖）。
- `subset<S: AsRef<str>>(&self, names: &[S]) -> Result<Registry, String>` — 按名挑子集，缺失时 `Err(缺失名)`。

### `impl Tool for ReadFile`

- `name()` → `"read_file"`。
- `definition()` — 单参数 `file_path`（String，必填），`strict: false`。
- `call(arguments)` — 反序列化 `ReadFileArgs`；解析失败或读取失败均记录 `error!` 日志并返回错误文本；成功返回文件内容（`tokio::fs::read_to_string`）。

### `impl Tool for WriteFile`

- `name()` → `"write_file"`。
- `definition()` — 参数 `file_path` + `file_content`（均 String，必填），`strict: false`。
- `call(arguments)` — 解析参数后 `tokio::fs::write` 覆盖写；成功返回 `wrote {len} bytes to {path}`，失败返回错误文本并记日志。

### `impl Tool for DeleteFile`

- `name()` → `"delete_file"`。
- `definition()` — 单参数 `file_path`（String，必填），`strict: false`。
- `call(arguments)` — `tokio::fs::remove_file` 删除；成功返回 `deleted file {path}`，失败返回错误文本并记日志。

### `impl Tool for RunCommand`

- `name()` → `"run_command"`。
- `definition()` — 单参数 `command`（String，必填），`strict: false`。
- `call(arguments)` — 通过 `sh -c` 执行命令（stdin 置 null、`kill_on_drop(true)`），用 `tokio::time::timeout(self.timeout, ...)` 包裹；超时返回 `command timed out after {n}s`；正常结束返回 `exit code: ...\nstdout:\n...\nstderr:\n...`（被信号终止时 status 为 `terminated by signal (...)`）。**无沙箱，以宿主权限运行**。
- `impl Default` — 超时默认 120 秒。

### 自由函数

- `builtin_registry()`（`tools/file_tools.rs:17`）— 一次性构造并注册全部 4 个内置工具（`RunCommand` 用 `default()` 的 120s 超时）。

## 1.3 对外 API 面

`pub use tool::{Registry, Tool}`；`tools::file_tools` 另 re-export `DeleteFile / ReadFile / RunCommand / WriteFile` 及 `builtin_registry()`。

---

# 2. `model` crate — 大模型协议适配

18 个 src 文件。结构：协议无关的 `core` + 厂商 `providers`（OpenAI 系含 DeepSeek 预设、Anthropic、Embedding、SSE）+ 顶层分派枚举 `Model`。

## 2.1 core 层（协议中立）

### `Error`（`core/error.rs`）

`#[derive(Debug)]`，实现 `Display` / `std::error::Error` / `From<reqwest::Error>`。

- `Http(reqwest::Error)` — 传输错误。
- `Api { status: u16, body: String }` — 非 2xx（保留响应体）。
- `EmptyResponse` — 无可用内容。
- `Protocol(String)` — 响应解析失败。

自由函数：`check_status(response)`（pub(crate)）— 非 2xx 时读 body 包成 `Error::Api`。

### `Message`（`core/message.rs:16`）

`Serialize, Deserialize, Debug, Clone, PartialEq, Eq`，`#[serde(tag = "role")]`。

- `System { content: String }`
- `User { content: String }`
- `Assistant { content: Option<String>, reasoning: Option<String>, tool_calls: Vec<ToolCall> }`（reasoning 为 DeepSeek 思考内容）
- `Tool { call_id: String, content: String }`（call_id 对应 ToolCall::id）

方法：

- `system / user / assistant / tool(content...) -> Self` — 便捷构造器（`message.rs:42-68`）。
- `role(&self) -> &'static str` — wire 角色名。
- `text(&self) -> &str` — 取文本；纯工具调用 assistant 返回空串。
- `impl From<ChatOutput> for Message` — 模型输出回放进历史。

### trait `Model`（`core/message.rs:105`，内部名 `ModelTrait`）

关联项为 3 个返回 `impl Future + Send` 的方法，无关联类型：

- `chat(&self, messages, user_id, response_format) -> Result<ChatOutput, Error>`
- `chat_stream(&self, ...) -> Result<ChatStream, Error>`
- `get_models(&mut self) -> Result<(), Error>`

### `ChatOutput`（`core/output.rs:7`）

`Debug, Clone, Default, PartialEq, Eq`。字段：`content: Option<String>`、`reasoning: Option<String>`、`tool_calls: Vec<ToolCall>`、`finish_reason: String`、`usage: Usage`。

### `Usage`（`core/output.rs:27`）

`Debug, Clone, Copy, Default, PartialEq, Eq`。字段：`prompt_tokens / completion_tokens / total_tokens: u32`。

方法：`new(prompt, completion)`（total 自动求和）；`merge(&mut self, other)`（逐字段取大合并流式累计用量快照）。

### `ToolCall`（`core/output.rs:59`）

`Serialize, Deserialize, Debug, Clone, PartialEq, Eq`。字段：`id / name / arguments: String`（arguments 为原始 JSON 串）。

### `ToolCallDelta`（`core/output.rs:75`）

`Debug, Clone, Default, PartialEq, Eq`。字段：`index: u32` + id/name/arguments 三个 `Option<String>`（增量碎片）。

### `ChatChunk`（`core/output.rs:87`）

`Debug, Clone, Default, PartialEq, Eq`。字段：`content / reasoning: Option<String>`、`tool_calls: Vec<ToolCallDelta>`、`finish_reason: Option<String>`、`usage: Option<Usage>`。

### `ChatStream`（`core/stream.rs:7`）

类型别名：`futures::stream::BoxStream<'static, Result<ChatChunk, Error>>`。

### `StreamAccumulator`（`core/stream.rs:15`）

`Debug, Default`。私有字段：`content / reasoning: String`、`tool_calls: BTreeMap<u32, PartialToolCall>`、`finish_reason: Option<String>`、`usage: Usage`。

- 私有辅助 `PartialToolCall`（`stream.rs:24`）：`id / name / arguments: String`。

方法：

- `new() -> Self`。
- `push(&mut self, chunk: &ChatChunk)` — 并入一个 chunk（文本拼接、工具调用按 index 归并、用量 merge、记录 finish_reason）。
- `finish(self) -> Result<ChatOutput, Error>` — 产出完整结果；空内容报 `EmptyResponse`、缺工具名报 `Protocol`、空参数规范化为 `{}`。

自由函数：`collect_stream(stream, on_chunk)` — 消费整条流并回调转发。

### `Function`（`core/tool.rs:12`）

`Serialize, Deserialize, Debug, Clone`。字段：`name / description: String`、`parameters: Vec<Parameter>`、`strict: bool`（OpenAI 严格模式）。

方法：`schema_object(&self) -> serde_json::Value` — 由 parameters 生成 `{type:object, properties, required}` JSON Schema。

### `Parameter`（`core/tool.rs:60`）

`Serialize, Deserialize, Debug, Clone` + 手写 `Default`。字段：`name / description: String`、`type: ParameterType`、`required: bool`（`#[serde(skip)]`，默认 true）。

方法：`required / optional(name, desc, type) -> Self` 构造器；`impl Default`（空名空描述、String、required=true）。

### `ParameterType`（`core/tool.rs:105`）

`Serialize, Deserialize, Debug, Clone`。单元变体 `String / Integer / Number / Boolean / Array / Object`（serde rename 为 JSON Schema 类型名）。

### `percent` 模块（`core/percent.rs`，无结构体）

`serialize` / `deserialize` 两个 serde helper，把 u8（百分之一定点数 0–200 ↔ 0.0–2.0）不经浮点地序列化为 JSON number；私有 `to_decimal_string(u8) -> String`。

## 2.2 顶层

### `Model`（`model.rs:13`）

`#[derive(Clone)]`。变体：`OpenAi(OpenAiModel)` / `Anthropic(AnthropicModel)`。按协议统一封装。

方法：`chat(...)` / `chat_stream(...)` / `get_models(&mut self)`（按变体分派到内部客户端）；`model_list(&self) -> &[String]`（已拉取的模型 id 列表）；`set_tools(&mut self, tools: &[Function])`（覆盖工具列表，翻译成各协议 wire 格式缓存）。

## 2.3 providers::config

### `Provider`（`config.rs:11`）

`Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq`，snake_case。变体：`Openai / Deepseek / Anthropic`。

### `ModelConfig`（`config.rs:25`）

`Serialize, Deserialize, Debug, Clone, PartialEq`（api_key 字段 `skip_serializing`）。字段：`provider: Provider`、`model: String`、`base_url: String`、`api_key: Option<String>`、`api_key_env: Option<String>`、`temperature / top_p: Option<f32>`、`max_tokens: Option<u32>`（仅 Anthropic）、`thinking: Option<bool>`（仅 DeepSeek）。

方法：`resolve_api_key() -> Result<String, ConfigError>`（api_key 优先，否则读 api_key_env 环境变量）；`build() -> Result<crate::Model, ConfigError>`（校验采样范围、按 provider 构造 OpenAi/DeepSeek/Anthropic 运行时模型）。私有 `to_percent(f32, &'static str)` — 小数 → 定点数并校验 0–2.0。

### `ConfigError`（`config.rs:60`）

`Debug, Clone, PartialEq, Eq` + `Display` + `Error`。变体：`MissingApiKey(String)`、`OutOfRange(&'static str)`。

## 2.4 providers::embedding

### `EmbeddingModel`（`embedding.rs:10`）

`Clone`。字段：`base_url / api_key: String`（私有）、`model: String`、`dimensions: Option<u32>`（pub）、`client: reqwest::Client`。

方法：`new(base_url, api_key, model)`（去尾斜杠）；`dimensions(u32) -> Self`（builder）；`embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, Error>`（POST /embeddings，校验数量、按 index 排序）。

私有 wire 类型：`EmbeddingRequest<'a> { model, input: &[String], dimensions, encoding_format: &'static str }`（Serialize）；`EmbeddingResponse { data: Vec<EmbeddingItem> }`、`EmbeddingItem { index: usize, embedding: Vec<f32> }`（Deserialize）。

## 2.5 providers::sse（crate 私有 `pub(crate)`）

### `SseEvent`（`sse.rs:13`）

`Debug, Clone, PartialEq, Eq`。字段：`event: Option<String>`、`data: String`。

### `SseParser`（`sse.rs:20`）

无 derive。字段：`buf: Vec<u8>`、`pending: VecDeque<SseEvent>`、`cur_event: Option<String>`、`cur_data: String`、`has_data: bool`。

方法：`new()`；`push(&mut self, bytes: &[u8])`（按行切分解析，UTF-8 安全）；`finish(&mut self)`（冲刷末尾无换行行与未分发事件）；`next_event(&mut self) -> Option<SseEvent>`；私有 `process_line(&str)`（处理 event:/data:/空行）、`dispatch()`（事件边界入队）。

自由函数：`sse_events<B, S>(byte_stream: S) -> impl Stream<Item = Result<SseEvent, Error>> + Send` — reqwest 字节流 → SSE 事件流桥。

## 2.6 providers::openai

### `OpenAiModel`（`openai/client.rs:584`）

`Clone`。字段：`base_url / api_key: String`（私有）、`model_list: Vec<String>`（pub）、`client: Client`、`args: OpenAiArgs`（pub）。

方法：`new(base_url, api_key, args)`（空 URL 回退 `OPENAI_BASE_URL`）；`with_default_url(...)`（pub(super)，供 DeepSeek 预设复用）；`deepseek(base_url, api_key, args)`（`deepseek.rs:16`，回退 `DEEPSEEK_BASE_URL`）；私有 `send_chat(&self, messages, user_id, response_format, stream) -> Result<reqwest::Response, Error>`（构造并发送 /chat/completions）；`impl ModelTrait`：`chat`（解析 ChatResponse → ChatOutput）、`chat_stream`（SSE 流 → ChatStream）、`get_models`（拉 /models 填 model_list）。

私有助手：`wire_messages(messages, echo_reasoning) -> Vec<WireMessage>`（中立消息 → OpenAI wire；echo_reasoning 控制 DeepSeek 工具回合回传 reasoning_content）；`no_tools(&&[OpenaiTool])`（serde skip 谓词）；`map_stream_event(event) -> Option<Result<ChatChunk, Error>>`（SSE 事件 → 中立 chunk；`[DONE]`/空帧返回 None）。

### `OpenAiArgs`（`client.rs:725`）

`Clone, Debug`。字段：`model: String`、`temperature: u8`（0–200 定点）、`top_p: u8`（0–100 定点）、`tools: Arc<[OpenaiTool]>`（pub(super)）、`vendor: OpenAiVendor`（pub）。

方法：`openai(model) -> Self`（默认采样 1.0、原生 OpenAI vendor）；`deepseek(model) -> Self`（vendor=Deepseek、思考开启 high）；builder `temperature(u8) / top_p(u8) / tools(&[Function]) / tool_choice(ToolChoice) / penalties(frequency, presence)`（后两者对非 OpenAi vendor 空操作）；`set_tools(&mut self, tools: &[Function])`（翻译并缓存 Arc 切片）；`thinking(t, effort)`（设 DeepSeek 思考模式/强度，非 DeepSeek vendor 空操作）；`build_request(...) -> ChatRequest`（pub(crate)，全借用构造请求体）。

### `OpenAiVendor`（`client.rs:166`）

`Serialize, Debug, Clone`，`untagged`（flatten 进请求顶层）。

- `OpenAi { frequency_penalty: Option<f32>, presence_penalty: Option<f32>, tool_choice: Option<ToolChoice> }`
- `Deepseek { thinking: Thinking, reasoning_effort: ThinkingConfig }`

### `Thinking`（`client.rs:186`）/ `ThinkingType`（`client.rs:206`）/ `ThinkingConfig`（`client.rs:217`）

均 `Serialize, Deserialize, Debug, Clone, Default`。`Thinking { type: ThinkingType }`；`ThinkingType { Disabled / Enabled }`（Default=Disabled）；`ThinkingConfig { High / Max }`（Default=High）。`Thinking::disabled() / enabled()` 构造器。

### `ResponseFormat`（`client.rs:231`）

`Serialize, Deserialize, Debug, Clone, PartialEq, Eq`，tag=type。变体：`Text / JsonObject`。方法：`parse(fmt: &str)`（`"json_object"` 识别，其余回退 Text）。

### `ToolChoice`（`client.rs:252`）

`Debug, Clone, PartialEq, Eq` + 手写 `Serialize`/`Deserialize`（wire 是字符串或对象）。变体：`Auto / Required / None / Function { name: String }`。

### `OpenaiTool`（`client.rs:331`）

`Serialize, Debug, Clone`。私有字段 `kind: &'static str`、`function: OpenaiFunctionDef`。`From<&Function>` 生成（翻译工具定义）。私有 `OpenaiFunctionDef`：`name / description: String`、`parameters: serde_json::Value`、`strict: bool`。

### wire 类型（openai）

- 请求（私有 Serialize，全借用）：`ChatRequest<'a>`（含 messages/model/response_format/stream/stream_options/temperature/top_p/tools/user + flatten vendor）、`StreamOptions { include_usage: bool }`、`WireMessage<'a>`、`WireToolCall<'a>`、`WireFunctionCall<'a>`。
- 响应（均 `Deserialize, Debug`，多数带 `#[allow(dead_code)]`）：`ChatResponse { id, object, created, model, system_fingerprint, choices, usage }`、`Choice { index, message, finish_reason, logprobs }`、`ResponseMessage { role, content, reasoning_content, tool_calls }`、`ToolCall { id, type, function }` + `ToolCallFunction { name, arguments }`、`Logprobs / LogprobContent / TopLogprob`、`Usage`（3 个 token 计数 + `prompt_cache_hit/miss_tokens`（DeepSeek）+ `completion_tokens_details`）、`CompletionTokensDetails { reasoning_tokens }`。
- 流式（私有 Deserialize）：`StreamResponse`、`StreamUsage`、`StreamChoice`、`StreamDelta`、`StreamToolCall`、`StreamToolCallFunction`。

## 2.7 providers::anthropic

### `AnthropicModel`（`anthropic/client.rs:24`）

`Clone`。字段：`base_url / api_key / version: String`（私有）、`model_list: Vec<String>`（pub）、`client: Client`、`args: AnthropicArgs`（pub）。

方法：`new(base_url, api_key, args)`（空 URL 回退 `ANTHROPIC_BASE_URL`，version=DEFAULT_VERSION）；`version(mut self, version)`（覆盖 anthropic-version 头）；私有 `build_request(messages, user_id, stream) -> MessagesRequest`（System 消息提取合并到顶层 system 字段）；私有 `send(request)`（POST /v1_messages，x-api-key 头）；`impl ModelTrait`：`chat`（text 块拼接、tool_use 转 ToolCall、stop_reason→finish_reason、usage 转换）、`chat_stream`（SSE 流）、`get_models`（拉 /v1/models）。response_format 参数被忽略（结构化输出走工具）。

### `AnthropicArgs`（`client.rs:212`）

`Clone, Debug` + 手写 `Default`（max_tokens=1024, temperature/top_p=100）。字段：`model: String`、`max_tokens: u32`、`system: Option<String>`、`temperature / top_p: u8`、`tools: Arc<[AnthropicTool]>`（私有）。

方法：`new(model)`、`max_tokens(u32)`、`system(impl Into<String>)`、`temperature(u8)`、`top_p(u8)`、`tools(&[Function])` builder；`set_tools(&mut self, ...)`。

### `AnthropicTool`（`client.rs:193`）

`Serialize, Debug, Clone`。私有 `name / description: String`、`input_schema: serde_json::Value`。`From<&Function>` 生成。

### wire 类型（anthropic）

- 请求（私有 Serialize）：`MessagesRequest<'a>`、`Metadata<'a> { user_id }`、`WireMessage<'a> { role, content: Vec<WireBlock> }`、`WireBlock<'a>`（tag=type）：`Text{text} / ToolUse{id,name,input:&RawValue} / ToolResult{tool_use_id,content}`。
- 响应/流式（私有 Deserialize）：`MessagesResponse`、`ResponseContentBlock`（tag=type + `#[serde(other)] Other`）；流式 `ContentBlockStart` / `ContentBlockStartBody`、`ContentBlockDelta` / `ContentBlockDeltaBody`（Text/InputJson/Thinking/Other）、`MessageDelta` / `MessageDeltaBody`、`MessageStart` / `MessageStartBody`、私有 `Usage{input_tokens, output_tokens}`（→ `impl From<Usage> for base::Usage`）、`ModelsResponse`、`ModelItem{id, display_name}`。

私有助手：`empty_input() -> &'static RawValue`（静态 `{}` 兜底非法 JSON 参数）；`wire_messages(&[Message]) -> Vec<WireMessage>`（角色映射、block 转换、相邻同角色合并、丢弃空 block）；`text_block(&str) -> Option<WireBlock>`；`parse_event<T: DeserializeOwned>(data)`；`map_stream_event(SseEvent)`（按 event 名分发 content_block_start / content_block_delta / message_start / message_delta，其余忽略）。

## 2.8 组合关系

- `Model`（顶层）持有 `OpenAiModel` | `AnthropicModel`。
- `OpenAiModel` / `AnthropicModel` 各持有自己的 Args、`Vec<String> model_list`、`reqwest::Client`。
- `OpenAiArgs` 持有 `OpenAiVendor`（第二层 vendor 差异字段）+ `Arc<[OpenaiTool]>`；`AnthropicArgs` 持有 `Arc<[AnthropicTool]>`。
- `OpenaiTool` / `AnthropicTool` 由 `Function`（经 `schema_object` 生成 parameters/input_schema）翻译而来；`Function` 持有 `Vec<Parameter>`（含 `ParameterType`）。
- `ChatOutput` 持有 `Vec<ToolCall>` + `Usage`；`ChatChunk` 持有 `Vec<ToolCallDelta>` + `Option<Usage>`；`Message::Assistant` 持有 `Vec<ToolCall>`。
- `StreamAccumulator` 持有 `BTreeMap<u32, PartialToolCall>`，产出 `ChatOutput`；`ChatStream` 是 `BoxStream<Result<ChatChunk, Error>>`。
- `ModelConfig → build() → 顶层 Model`；`SseParser → SseEvent` 队列，被两个客户端的流式实现共用。

## 2.9 对外 API 面（`lib.rs` 的 pub use）

- 统一入口：`Model`（顶层枚举）。
- 协议中立层：`collect_stream, ChatChunk, ChatOutput, ChatStream, Error, Function, Message, Model as ModelTrait, Parameter, ParameterType, StreamAccumulator, ToolCall, ToolCallDelta, Usage`。
- 配置与向量化：`ConfigError, ModelConfig, Provider, EmbeddingModel`。
- OpenAI 系：`OpenAiArgs, OpenAiModel, OpenAiVendor, OpenaiTool, ResponseFormat, Thinking, ThinkingConfig, ThinkingType, ToolChoice, DEEPSEEK_BASE_URL, OPENAI_BASE_URL`。
- Anthropic：`AnthropicArgs, AnthropicModel, AnthropicTool, ANTHROPIC_BASE_URL`（注意：`DEFAULT_MAX_TOKENS`、`DEFAULT_VERSION` 只在 `providers::anthropic` 模块层 re-export，未进 crate 根）。

隐藏的内部面：`percent` 序列化模块、`sse` 模块、各协议的请求/响应 wire 类型、`check_status`。`ChatResponse` 等响应类型虽在 `providers::openai` 模块路径上 pub，但未在 crate 根导出。

---

# 3. `agent` crate — 三层 agent 系统

7 个 src 文件。定位：L1 主控 / L2 worker / L3 子 agent，L1 队列驱动，L2/L3 共用 ReAct 执行器。依赖 `model`（Model/Message/Usage）、`plugin`（Registry/Tool）。

## 3.1 数据结构

### `AgentError`（`lib.rs`，enum，Debug）

运行错误。变体：`Model(model::Error)`（底层模型调用失败）、`MaxTurnsExceeded`（超最大轮次）、`Config(String)`（配置引用不存在）。实现 `Display`、`Error`（source 透传 Model）、`From<model::Error>`。

### 配置（`core/config.rs`，均 `Serialize, Deserialize, Debug, Clone`）

**`AgentConfig`** — 整个系统配置。字段：`controller: ControllerSpec`、`workers: Vec<WorkerSpec>`（L2 种类，serde default）、`limits: Limits`（serde default）。

**`ControllerSpec`** — L1 主控。字段：`model: String`（模型池 key）、`system: Option<String>`（追加提示）、`tools: Vec<String>`（额外真实工具，默认空）、`max_turns: usize`（默认 8）。

**`WorkerSpec`** — L2 一类工作 agent。字段：`name: String`（唯一名）、`description: String`（能力描述）、`model: String`、`system: String`、`tools: Vec<String>`、`can_spawn: bool`（默认 true，能否开 L3）、`max_turns: usize`（默认 30）。

**`Limits`** — 全局限制。字段：`max_concurrent_tasks: usize`（默认 4）、`subagent_max_turns: usize`（默认 30）、`max_tool_output_chars: usize`（默认 32KB）。手写 `Default`。文件内私有辅助：`default_true / default_controller_turns / default_worker_turns / default_concurrent / default_tool_output`。

### 事件（`core/event.rs`）

**`AgentEvent`**（enum，`Serialize, Debug, Clone, PartialEq`，`#[serde(tag="type", rename_all="snake_case")]`）— 会话内事件，`agent` 字段为层级路径标签：

- `TextDelta { agent, text }` / `ReasoningDelta { agent, text }` — 流式文本/推理碎片
- `ToolCall { agent, id, name, arguments }` / `ToolResult { agent, id, name, output }`（output 已截断）
- `Usage { agent, usage: UsageView }` — 单轮用量
- `TaskStarted { task_id: u64, worker, task }` / `TaskFinished { task_id, worker, result: Option<String>, error: Option<String> }` — 任务生命周期
- `Reply { text }` — 主控给用户的最终回复
- `Error { message }` — 主控处理出错

**`UsageView`**（struct，`Serialize, Debug, Clone, Copy, PartialEq, Eq`）— `prompt_tokens / completion_tokens / total_tokens: u32`；`impl From<model::Usage>` 转换。

**`EventObserver`**（trait，`Send + Sync`）— 无损同步观察者：`observe(&self, session_id: &str, event: &AgentEvent)`。

**`EventSender`**（struct，`Clone`）— 事件出口。字段：`session_id: Arc<str>`、`broadcast: broadcast::Sender<AgentEvent>`、`observer: Option<Arc<dyn EventObserver>>`。

### 执行器（`core/runner.rs`）

**`Runner`**（struct，`Clone`）— ReAct 执行器。字段：`label: String`（pub，事件标签）、`model: model::Model`、`registry: plugin::Registry`、`max_turns: usize`、`max_tool_output_chars: usize`、`events: EventSender`。

### 编排（`orchestration/session.rs`）

**`Input`**（enum，`Debug, Clone, PartialEq, Eq`）— 主控队列输入。变体：`User(String)`（用户消息）、`Todo(String)`（到期待办）、`TaskDone { task_id: u64, worker: String, result: Result<String, String> }`（任务结果）。

**`HistorySink`**（trait，`#[async_trait] Send + Sync`）— 历史持久化出口：`async fn save(&self, session_id: &str, history: &[model::Message])`。

**`SessionOptions`**（struct，`Default`）— 启动选项。字段：`history: Vec<Message>`（恢复的历史，system 会被丢弃）、`tools: plugin::Registry`（会话专属工具）、`sink: Option<Arc<dyn HistorySink>>`、`observer: Option<Arc<dyn EventObserver>>`、`first_task_id: u64`（任务 id 起点，0 视为 1）。

**`TaskInfo`**（struct，`Debug, Clone, PartialEq, Eq, Serialize`）— 任务概况：`task_id: u64`、`worker: String`、`task: String`。

**`AgentSystem`**（struct，`Clone` 廉价）— 系统入口。字段：`inner: Arc<SystemInner>`。

**`SystemInner`**（struct，`pub(crate)`）— 字段：`config: AgentConfig`、`models: HashMap<String, Model>`（模型池）、`tools: Registry`（工具池）。

**`SessionHandle`**（struct，`Clone`）— 会话句柄。字段：`inbox: mpsc::UnboundedSender<Input>`、`events: EventSender`、`shared: Arc<Session>`。

**`Session`**（struct，`pub(crate)`）— 会话共享状态。字段：`system: Arc<SystemInner>`、`session_id: String`、`inbox: mpsc::WeakUnboundedSender<Input>`（弱引用防引用环）、`events: EventSender`、`next_task_id: AtomicU64`、`tasks: Mutex<BTreeMap<u64, RunningTask>>`、`session_tools: Registry`（pub(crate)）。

**`RunningTask`**（struct，私有）— 字段：`info: TaskInfo`、`abort: tokio::task::AbortHandle`。

常量：`pub const NO_REPLY: &str` — 主控回复此标记表示无需告知用户。

### 元工具（`orchestration/tools.rs`，全部私有，仅经 Registry 暴露给模型）

- `DispatchTask` / `ListTasks` / `CancelTask` / `AddTodo` — 均为 tuple struct 持有 `Arc<Session>`；均 `impl plugin::Tool`。常量 `MAX_TODO_DELAY: u64 = 7 天`。
- `DispatchArgs`（Deserialize）— `worker: String`、`task: String`。
- `CancelArgs`（Deserialize）— `task_id: u64`。
- `AddTodoArgs`（Deserialize）— `text: String`、`delay_seconds: u64`（serde default）。
- `SpawnArgs`（Deserialize）— `task: String`。
- `SpawnSubagent` — 字段：`session: Arc<Session>`、`spec: WorkerSpec`、`parent: String`、`next: AtomicU32`（子任务编号）；`impl Tool`。宏 `parse_args!`（参数解析失败时把错误文本直接作为工具输出返回）。

## 3.2 方法清单

### `impl AgentError`

- `Display::fmt` — 按变体格式化错误消息。
- `Error::source` — `Model` 变体透传底层错误。
- `From<model::Error>::from` — 包装为 `AgentError::Model`。

### `impl Default for Limits`

`default` — 三个字段取各自默认值（4 / 30 / 32768）。

### `impl From<Usage> for UsageView`

`from` — 拷贝三个 token 计数字段。

### `impl EventSender`

- `new(capacity: usize) -> Self` — 只有 broadcast、无观察者的出口（单独用 Runner 时）。
- `with_observer(session_id: &str, capacity: usize, observer: Option<Arc<dyn EventObserver>>) -> Self` — 创建 broadcast channel（容量 capacity）并挂上可选观察者。
- `send(&self, event: AgentEvent)` — 先同步调观察者（无损），再 broadcast（无订阅者时静默丢弃）。
- `subscribe(&self) -> broadcast::Receiver<AgentEvent>` — 订阅之后产生的事件。

### `impl Runner`

- `new(label, mut model, registry, max_turns, max_tool_output_chars, events) -> Self` — 把注册表工具定义装到模型（`model.set_tools`）后构造执行器。
- `run(&self, history: &mut Vec<Message>) -> Result<String, AgentError>` — 核心 ReAct 循环（最多 max_turns 轮）：流式调用模型、碎片经 `forward` 实时发事件、发 Usage；无工具调用则返回最终文本；否则本轮多个工具调用用 `join_all` 并发执行、结果按调用顺序回灌 history；中途出错也保持 history 协议合法。
- `call_tool(&self, call: &ToolCall) -> String`（私有）— 发 ToolCall 事件，调 `registry.call`，未知工具返回提示文本，输出按字符截断后发 ToolResult 事件并返回。
- `forward(&self, chunk: &ChatChunk)`（私有）— 把流式 chunk 的 content/reasoning 转成 TextDelta/ReasoningDelta 事件。
- `emit(&self, event: AgentEvent)`（pub(crate)）— 经 events 发出事件（被 orchestration 层复用）。
- 自由函数 `truncate(text, max_chars) -> String`（pub(crate)）— 按字符边界截断并追加 `...[truncated N chars]`（含单元测试）。

### `impl Input`（私有）

- `render(&self) -> String` — 渲染成交给主控模型的文本（User 原文、Todo 包 `<todo_due>`、TaskDone 包 `<task_result … status="ok|failed">`）。

### `impl AgentSystem`

- `new(config: AgentConfig, models: HashMap<String, Model>, tools: Registry) -> Result<Self, AgentError>` — 校验配置引用的模型/工具都存在、worker 名不重复，否则返回 `AgentError::Config`；通过后装入 `Arc`。
- `config(&self) -> &AgentConfig` — 读配置。
- `start_session(&self, session_id, options: SessionOptions) -> SessionHandle` — 建 mpsc 无限队列与 EventSender（容量 1024）、构造 `Session` 共享状态（task id 从 `first_task_id.max(1)` 起）、`tokio::spawn` 主控循环，返回句柄；所有句柄被丢弃后会话结束并取消在跑任务。

### `impl SessionHandle`

- `session_id(&self) -> &str`
- `send(&self, input: Input) -> bool` — 投递输入；会话已结束返回 false。
- `send_user(&self, text) -> bool` — 便捷方法：投递 User 输入。
- `subscribe(&self) -> broadcast::Receiver<AgentEvent>` — 订阅事件。
- `tasks(&self) -> Vec<TaskInfo>` — 列出在跑任务。
- `cancel_task(&self, task_id: u64) -> bool` — 取消任务（与主控 `cancel_task` 工具等效）。

### `impl Session`（全部 `pub(crate)` 或私有）

- `worker(&self, name: &str) -> Option<&WorkerSpec>` — 按名找 worker 规格。
- `tasks(&self) -> Vec<TaskInfo>` — 锁任务表后克隆全部 TaskInfo。
- `emit(&self, event: AgentEvent)` — 发事件。
- `push_input(&self, input: Input)` — 弱引用升级后塞回主控队列（任务结果回灌用）。
- `dispatch(self: &Arc<Self>, worker: &str, task: String) -> Result<u64, String>` — 派单：校验 worker 存在与并发上限（`max_concurrent_tasks`），分配 task_id，`worker_runner` 构造 Runner 后 `tokio::spawn` 独立任务（system + user 初始 history，跑完调 `finish`），登记 RunningTask（含 AbortHandle），发 TaskStarted。
- `finish(&self, task_id: u64, result: Result<String, String>)` — 任务结束：从任务表移除（已取消则无操作）、发 TaskFinished、结果以 `Input::TaskDone` 回灌主控队列。
- `cancel(&self, task_id: u64) -> bool` — 移除并 abort 任务，发 error="cancelled" 的 TaskFinished。
- `schedule_todo(&self, text: String, delay: Duration)` — spawn 定时任务，delay 后把 `Input::Todo` 塞入队列。
- `worker_runner(self: &Arc<Self>, spec: &WorkerSpec, label: String) -> Runner` — 构造 L2 执行器：模型克隆 + `worker_registry`（含 spawn_subagent）+ spec.max_turns。
- `subagent_runner(&self, spec: &WorkerSpec, label: String) -> Runner` — 构造 L3 执行器：工具为 spec.tools 子集 + 会话工具（不再派生），轮次用 `subagent_max_turns`。
- `abort_all(&self)` — 取出并 abort 全部在跑任务（主控循环退出时调用）。

自由函数（私有）：

- `runner_system(spec: &WorkerSpec) -> String` — worker system 提示，can_spawn 时追加 spawn_subagent 说明。
- `controller_system(session: &Session) -> String` — 主控 system 提示：职责说明 + worker 清单 + NO_REPLY 约定 + 配置追加提示。
- `controller_loop(session: Arc<Session>, rx, restored, sink)` — 主控循环：重建 system 消息 + 过滤后的恢复历史；每次取一批积压输入合并渲染，跑 Runner；成功则 trim 后非空非 NO_REPLY 时发 Reply；失败则回滚 history 到本批之前并写入 error 占位（保持协议合法）、发 Error 事件；每批后调 `sink.save`；队列关闭（句柄全丢）后 `abort_all`。

### `orchestration/tools.rs`

- `controller_registry(session: &Arc<Session>) -> Registry`（pub(crate)）— 主控工具注册表：配置允许的工具子集 + 会话工具 + DispatchTask/ListTasks/CancelTask/AddTodo 四个元工具。
- `worker_registry(session, spec, label) -> Registry`（pub(crate)）— L2 工具注册表：spec.tools 子集 + 会话工具；can_spawn 时追加 SpawnSubagent。
- `string_param(name, description)` — 构造必填字符串参数。

每个工具 struct 的 `impl Tool`（async_trait）：

- `DispatchTask` — `name`="dispatch_task"；`definition` 列出可选 worker 名，参数 worker/task；`call` 解析后调 `Session::dispatch`，返回 task id 或错误文本。
- `ListTasks` — `name`="list_tasks"；`call` 返回在跑任务列表或 "no running tasks"。
- `CancelTask` — `name`="cancel_task"；`call` 调 `Session::cancel`，返回取消结果。
- `AddTodo` — `name`="add_todo"；`call` 把 delay 钳制到 7 天内，调 `schedule_todo`，返回 "todo scheduled in Ns"。
- `SpawnSubagent` — `name`="spawn_subagent"；`call` 编号 `parent/sub{n}`，用 `subagent_runner` 在 system+user 初始历史上同步跑完，返回最终报告或 "sub-agent failed: …"。

## 3.3 组合关系

- `AgentSystem` →（`Arc`）→ `SystemInner` → `AgentConfig`、`HashMap<String, Model>`（模型池）、`Registry`（工具池）。
- `SessionHandle` →（`Arc`）→ `Session`；`Session` → `Arc<SystemInner>`、`WeakUnboundedSender<Input>`（防引用环）、`EventSender`、`Mutex<BTreeMap<u64, RunningTask>>`、`Registry`（会话专属工具）。
- `RunningTask` → `TaskInfo` + `AbortHandle`。
- 元工具 struct 全部持有 `Arc<Session>`；SpawnSubagent 另持有 `WorkerSpec` 克隆和编号器。
- `Runner` → `Model` + `Registry` + `EventSender`（由 `worker_runner`/`subagent_runner`/`controller_loop` 构造）。
- 事件流：`Runner`/`Session` → `EventSender` →（broadcast + 可选）`Arc<dyn EventObserver>`；`HistorySink` 由 `controller_loop` 每批处理后回调。

## 3.4 对外 API 面

`pub mod core`、`pub mod orchestration`（`tools` 是 `pub(crate)`，元工具不对外）；`pub use` 共 13 项：

- 配置：`AgentConfig`、`ControllerSpec`、`Limits`、`WorkerSpec`
- 事件：`AgentEvent`、`EventObserver`、`EventSender`、`UsageView`
- 执行器：`Runner`
- 会话：`AgentSystem`、`HistorySink`、`Input`、`SessionHandle`、`SessionOptions`、`TaskInfo`，以及常量 `NO_REPLY`

另：`AgentError` 直接定义在 lib.rs 根。

跨 crate 消费（server）：`AgentSystem::new`、`start_session`、`send_user`、`subscribe`、`tasks()`、`cancel_task`；`AgentConfig`；`HistorySink`、`SessionOptions`、`EventObserver`、`UsageView`；`AgentEvent` 全部 9 个变体被 tasks.rs 的观察者匹配消费、被 sessions.rs 序列化为 SSE。

---

# 4. `symbol` crate — 持久化记忆

10 个 src 文件（约 1550 行）。定位：符号侧工具 crate，提供持久化记忆器 `MemoryStore`，存储在 SurrealDB（内嵌 SurrealKV）：段落 + 实体/关系图（LLM 异步抽取）+ 三路检索融合 + 关系半衰期衰减。

## 4.1 配置与抽象

### `MemoryConfig`（`memory/config.rs`）

`Serialize, Deserialize, Debug, Clone` + `#[serde(default)]`，手写 `Default`。字段：

- `endpoint: String` — SurrealDB 地址（内嵌 surrealkv 或 `mem://`）
- `namespace: String` / `database: String` — namespace；database 即记忆空间
- `embedding_dimension: usize` — 向量维度，须与 embedding 模型一致
- `half_life_hours: f64` — 关系强度半衰期（小时）
- `inactive_threshold: f64` — 低于此有效强度则关系冻结
- `prune_threshold: f64` — `maintain` 时低于此则删除
- `min_confidence: f64` — 抽取关系最低置信度
- `retrieval: RetrievalConfig` — 三路检索融合参数（嵌套持有）

`impl Default`（endpoint `surrealkv://data/memory`、维度 1024、半衰期 7 天等）。

### `RetrievalConfig`

同 derive。字段：`semantic_top_k / sparse_top_k / graph_top_k: usize`、`semantic_weight / sparse_weight / graph_weight: f64`、`min_score: f64`、`relative_cutoff: f64`、`access_cooldown_ms: i64`。

### trait `Embedder`（`memory/embed.rs`，`Send + Sync`，`#[async_trait]`）

- `dimension(&self) -> usize`
- `async embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, MemoryError>`

### `ApiEmbedder`

无 derive。字段：`model: EmbeddingModel`（来自 `model` crate，私有）、`dimension: usize`。

方法：`new(model, dimension)`；`impl Embedder`：`dimension()`（返回期望维度）、`embed(inputs)`（调 `EmbeddingModel::embed` 并校验每行维度，不符报 `Invalid`）。

### `MemoryError`（`memory/error.rs`）

`#[derive(Debug)]`，手写 `Display` / `Error` / 三个 `From`。变体：`Db(surrealdb::Error)`、`Model(model::Error)`、`Invalid(String)`（数据/参数不合法）。

### trait `Extractor`（`memory/extract.rs`，`Send + Sync`，`#[async_trait]`）

`async extract(&self, text: &str) -> Result<Extraction, MemoryError>`。

### `LlmExtractor`

无 derive。字段：`model: Model`（私有）。方法：`new(model)`；`impl Extractor`：`extract(text)`（用固定 `EXTRACTION_PROMPT` 组 system+user 消息，`chat(..., Some("json_object"))` 调模型并 `parse_extraction` 解析输出）。

## 4.2 数据类型（`memory/types.rs`）

### `ExtractedEntity` / `ExtractedRelation` / `Extraction`

- `ExtractedEntity { name: String, kind: String }`（kind serde rename 为 `"type"`，默认空）。
- `ExtractedRelation { subject / predicate / object: String, confidence: f64 }`（confidence serde 默认 0.8）。
- `Extraction { entities: Vec<ExtractedEntity>, relations: Vec<ExtractedRelation> }`（+ Default）。

### `IngestRequest`

`Serialize, Deserialize, Debug, Clone, Default`。字段：`content: String`（记忆正文）、`source: String`（来源标签，可按来源批量删除）、`external_id: Option<String>`（调用方幂等键）、`tags: Vec<String>`、`extract: bool`（是否排队三元组抽取，serde 默认 true）。

方法：`new(content)`（extract 默认 true）；`source(...) / external_id(...) / tags(...)` builder 风格链式设值。

### `IngestResult`

`Serialize, Debug, Clone, PartialEq, Eq`。字段：`id: String`（段落 id，内容 hash）、`created: bool`（false 表示已存在只合并标签）。

### `MemoryHit`

`Serialize, Deserialize, Debug, Clone, PartialEq`。字段：`id / content / source: String`、`tags: Vec<String>`、`created_at_ms: i64`、`score: f64`（融合分）、`channels: ChannelScores`、`relations: Vec<String>`（支撑关系的可读标签）。

### `ChannelScores`

`Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq`。字段：`semantic / sparse / graph: f64`。

### `Memory`

`Serialize, Deserialize, Debug, Clone, PartialEq, Eq`；`From<ParagraphRow>` 转换。字段：`id / content / source: String`、`tags: Vec<String>`、`created_at_ms: i64`。

### `MemoryStats`

`Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq`。字段：`paragraphs / pending_extraction / entities / relations / inactive_relations: u64`。

### `ParagraphRow`（crate 私有）

`Deserialize, Debug, Clone`。字段：`id / content / source / tags / created_at_ms` — 对应 `paragraph` 表的行。

### `RelationRow`（crate 私有）

同 derive。字段：`id / subject / predicate / object: String`、`paragraphs: Vec<String>`（支撑段落 id 列表）、`strength: f64`（默认 1.0）+ `anchor_ms: i64`（惰性半衰期衰减参数对）、`pinned: bool`（钉住不衰减）。

### `RelationView`

`Serialize, Debug, Clone, PartialEq`。字段：`id / subject / predicate / object: String`、`strength: f64`（衰减后的有效强度）、`paragraphs: Vec<String>`。

### 其他

- `pub(crate) fn schema(dimension: usize) -> String` — 生成幂等的 SurrealDB schema（analyzer + paragraph/entity/relation 表 + BM25/HNSW 索引）。
- `impl From<ParagraphRow> for Memory` — 行 → 对外视图。
- `pub(crate) fn effective_strength(row, now_ms, half_life_hours) -> f64` — 半衰期衰减 `strength × 2^(-Δh/half_life)`；pinned 不衰减。

## 4.3 抽取函数（`memory/extract.rs`）

- `pub fn parse_extraction(raw: &str) -> Result<Extraction, MemoryError>` — 截取首个 `{` 到最后一个 `}` 之间的 JSON 并反序列化，容忍 ```json 包裹。
- `pub fn sanitize(extraction, min_confidence) -> Extraction` — 规范化名字、过滤空白/低置信度项、关系两端补进实体表、按规范化键去重取最大置信度。

## 4.4 text.rs（纯函数工具，全部 pub）

- `normalize(text) -> String` — 折叠连续空白。
- `content_hash(text) -> String` — sha256 前 16 字节 hex，作记录 id。
- `entity_key(name) -> String` — 小写 + 折叠空白，实体去重键。
- `relation_key(s, p, o) -> String` — 三元组规范化后的 content_hash，关系 id。
- `search_terms(text) -> String` — 检索分词：拉丁/数字按词小写，CJK 切字二元组，空格拼接。
- `query_terms(text) -> String` — 同上分词但去重，供 OR 查询。

## 4.5 `MemoryStore`（`memory/store.rs` + `retrieve.rs` 两个 impl 块）

仅 `#[derive(Clone)]`（Clone 廉价，共享连接）。字段：

- `db: Surreal<Any>` — SurrealDB 连接
- `config: Arc<MemoryConfig>` — 配置（Arc 共享）
- `embedder: Option<Arc<dyn Embedder>>` — 向量化器（None 则无语义通道）
- `extractor: Option<Arc<dyn Extractor>>` — 抽取器（None 则不抽三元组）

### store.rs 方法

- `open(config, embedder, extractor) -> Result<Self, MemoryError>` — 连接存储并初始化 schema。
- `with_connection(db, config, embedder, extractor)` — 复用已有 `Surreal<Any>` 连接（clone 独立 session 切 ns/db），校验 embedder 维度。
- `scoped(&self, database) -> Result<Self, _>` — 校验名字后切到同 namespace 下另一个 database（每用户一个记忆空间）。
- `config(&self) -> &MemoryConfig` — 读配置。
- `init_schema(&self)`（私有）— 执行 schema SQL。
- `query_rows<T: DeserializeOwned>(sql, vars)`（pub(crate)）— 执行多语句 SQL，取最后一条语句结果反序列化（供 retrieve.rs 使用）。
- `execute_vars(sql, vars)`（pub(crate)）— 只检查错误的执行。
- `query` / `execute`（私有薄封装）。
- `embed_one(text)`（私有）— 有 embedder 则单向量嵌入。
- `embed_query(text)`（pub(crate)）— 检索用查询向量。
- `get(id) -> Result<Option<Memory>, _>` — 按 id 取一条。
- `recent(limit) -> Result<Vec<Memory>, _>` — 最近写入，新的在前。
- `delete(id) -> Result<bool, _>` — 删段落并从关系支撑列表摘除；无支撑且未钉住的关系级联删除。
- `delete_by_source(source) -> Result<usize, _>` — 按来源批量删除。
- `detach_paragraphs(ids)`（私有）— 从 relations 摘掉段落并清理空关系。
- `stats() -> Result<MemoryStats, _>` — 五类计数。
- `ingest(request: IngestRequest) -> Result<IngestResult, _>` — 核心写入：规范化→`external_id` 幂等→内容 hash 去重（重复只合并 tags）→嵌入→建段落行，`extract` 时标记待抽取。
- `process_pending(limit) -> Result<usize, _>` — 处理待抽取段落：LLM 抽取→`sanitize`→`write_graph`，失败仅 warn 并标记完成。
- `write_graph(paragraph_id, extraction)`（私有）— 实体 UPSERT（mentions+1），关系存在则并入段落/重置强度，否则 RELATE 建边（含 terms、embedding、confidence、strength、anchor_ms 等）。

### retrieve.rs 方法

- `search(query, limit) -> Result<Vec<MemoryHit>, _>` — 核心检索：semantic（段落 KNN）+ sparse（段落 BM25 归一化）+ graph（关系 KNN+BM25，按有效强度加权展开到支撑段落，主关系分+0.35×次要关系分）→ 权重归一融合 → `max(min_score, 首条×relative_cutoff)` 截断 → 取正文 → 对命中关系做访问强化。
- `reinforce_access(ids, now)`（私有）— 先把衰减结算进 strength，再 `s += 0.05×(1−s)`，受冷却期限制，跳过 pinned。
- `maintain() -> Result<(usize, usize), _>` — 生命周期维护：低于 prune 阈值删除、低于 inactive 阈值冻结，返回 (冻结数, 删除数)。
- `pin_relation(id, pinned) -> Result<bool, _>` — 钉住/取消钉住（钉住后不衰减不清理）。
- `relations_of(entity) -> Result<Vec<RelationView>, _>` — 某实体的全部关系，按衰减后强度降序。
- `normalize_by_max(rows)`（模块私有）— BM25 按批内最高分归一化（全 0 时命中给满分）。
- `now_ms()`（pub(crate)）— 当前 Unix 毫秒。

模块私有类型：`Scored { id, score }`（KNN/BM25 命中行）；`ScoredRelation { #[serde(flatten)] row: RelationRow, score }`（带分数的关系行）。

## 4.6 记忆工具（`memory/tools.rs`，私有，经 Registry 暴露）

- `MemorySearch(MemoryStore)`（tuple struct）— `impl Tool`：`name()`→`"memory_search"`；`definition()`（query 必填、limit 可选）；`call(args)`→解析参数、`store.search`（limit clamp 1–20）、格式化为 `[id] (score) content + facts` 文本。
- `MemorySave { store: MemoryStore, source: String }` — `name()`→`"memory_save"`；`call()`→构造 `IngestRequest`（带工具 source 和 tags）调 `ingest`，返回 saved/already remembered。
- `MemoryForget(MemoryStore)`（tuple struct）— `name()`→`"memory_forget"`；`call()`→调 `delete`，返回 forgot/no memory。
- 参数：`SearchArgs { query, limit }` / `SaveArgs { content, tags }` / `ForgetArgs { id }`（私有，均 Deserialize）。
- `pub fn register_memory_tools(registry: &mut Registry, store: MemoryStore, source: &str)` — 注册三个 agent 工具。

## 4.7 组合关系

- `MemoryStore` 持有 `Surreal<Any>`、`Arc<MemoryConfig>` → `RetrievalConfig`、`Option<Arc<dyn Embedder>>`（生产实现 `ApiEmbedder` → `EmbeddingModel`）、`Option<Arc<dyn Extractor>>`（生产实现 `LlmExtractor` → `Model`）。
- DB 内逻辑持有：`paragraph` 行（`ParagraphRow`）被 `relation` 行的 `paragraphs` 字段反向引用；`Memory`/`MemoryHit`/`RelationView` 是这些行的对外只读视图。
- 抽取管线：`IngestRequest` →（`ingest` 排队）→ `LlmExtractor::extract` 产 `Extraction` → `sanitize` → `write_graph` 落库。
- 错误汇聚：所有方法返回 `Result<_, MemoryError>`，底层 `surrealdb::Error`、`model::Error`、`serde_json::Error` 经 `From` 自动上抛。

## 4.8 对外 API 面（lib.rs 的 pub use）

- `pub mod memory`（子模块全 pub），额外 `pub use memory::text`（整个 text 模块直接 re-export）。
- 类型/配置：`MemoryConfig`、`RetrievalConfig`、`MemoryError`、`MemoryStore`
- 抽取：`parse_extraction`、`ExtractedEntity`、`ExtractedRelation`、`Extraction`、`Extractor`、`LlmExtractor`
- 嵌入：`ApiEmbedder`、`Embedder`
- 工具：`register_memory_tools`
- 数据视图：`ChannelScores`、`IngestRequest`、`IngestResult`、`Memory`、`MemoryHit`、`MemoryStats`、`RelationView`

未 re-export 的公开项：`memory::extract::sanitize`、`memory::retrieve`、`memory::types`（除上述项）等只能经模块路径访问；`store.rs`/`retrieve.rs` 中的 `pub(crate)` 助手（`query_rows`/`execute_vars`/`embed_query`/`now_ms`）仅 crate 内部用。

---

# 5. `server` crate — HTTP 与组装

10 个 src 文件（约 2000 行）。纯库 crate，无 bin；`thought-core` 是唯一外部调用方，走 `lib.rs` 的公开 API。

## 5.1 组装入口（`src/lib.rs`）

### `BuildError(pub String)`

derive `Debug`。实现 `Display`、`std::error::Error`。组装期错误（启动失败直接给运维看）。

### `Built`

无 derive。字段：`pub router: axum::Router`（可直接 serve 的路由）、`pub state: AppState`（共享状态）。

### 自由函数

- `pub async fn build(config: &ServerConfig, tools: Registry) -> Result<Built, BuildError>` — 按配置组装后端。
- `pub async fn build_from_file(path: &Path, tools: Registry) -> Result<Built, BuildError>` — 从 TOML 文件组装；相对路径按配置文件目录解析，并启用配置读写接口。
- `build_agents(config, tools) -> Result<AgentSystem, BuildError>`（pub(crate)）— 由 `models` 配置构造模型池 + agent 系统（启动与配置热更新共用）。
- `fail(context, e)`（私有）— 错误包装。
- `build_inner(config, config_path, tools)`（私有）— 核心组装：Jwt → agents → Db → TaskRegistry → MemoryStore → AppState → router。
- `bootstrap_admin(config, db)`（私有）— 无任何用户时按配置创建管理员。
- `pub fn spawn_memory_worker(state: AppState, interval: Duration) -> JoinHandle<()>` — 周期性记忆维护后台任务。
- `pub async fn maintain_spaces(state: &AppState) -> Result<(), String>` — 对所有（或每用户）记忆空间跑一轮：抽取 ≤32 条待处理段落 + 衰减维护。

## 5.2 配置模型（`src/config.rs`，均 `Serialize, Deserialize, Debug, Clone`）

### `ServerConfig` — 顶层配置

字段：`http: HttpConfig`（serde default）、`auth: AuthConfig`、`storage: StorageConfig`（default）、`models: BTreeMap<String, ModelConfig>`（来自 `model` crate，模型池）、`embedding: Option<EmbeddingConfig>`（default）、`memory: Option<MemorySection>`（default）、`agent: AgentConfig`（来自 `agent` crate）。

方法：`resolve_paths(&mut self, base: &Path)` — 把相对路径（web_dir、surrealkv endpoint）改为相对 `base`。

### `HttpConfig`

字段：`bind: String`（默认 `127.0.0.1:8080`）、`web_dir: PathBuf`（默认 `web`）。`impl Default`。

### `AuthConfig`

字段：`jwt_secret: Option<String>`（skip_serializing）、`jwt_secret_env: Option<String>`、`token_ttl_secs: u64`（默认 12h）、`bootstrap_admin: Option<BootstrapAdmin>`。

### `BootstrapAdmin`

字段：`username: String`、`password: Option<String>`（skip）、`password_env: Option<String>`。

### `StorageConfig`

字段：`endpoint: String`（默认 `surrealkv://data/thought`）、`namespace: String`（`thought`）、`database: String`（`server`）。`impl Default`。

### `EmbeddingConfig`

字段：`base_url: String`、`model: String`、`dimension: usize`、`api_key: Option<String>`（skip）、`api_key_env: Option<String>`、`send_dimensions: bool`。

### `MemorySection`

字段：`store: MemoryConfig`（`#[serde(flatten)]`，来自 `symbol` crate）、`extractor_model: Option<String>`（抽取三元组用的模型名）、`per_user: bool`（默认 true，每用户一个记忆空间）、`save_user_messages: bool`（是否自动把用户消息写入记忆）。

### 自由函数

`config_dir(path) -> PathBuf`（配置文件所在目录）；`resolve_endpoint(endpoint, base)`（`surrealkv://相对路径` 前缀拼接）；`secret(plain, env) -> Option<String>`（明文或环境变量取密钥，滤空串，pub(crate)）。

## 5.3 认证（`src/core/auth.rs`）

### `Role`（enum）

`Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq`（snake_case）。变体：`Admin / User`。

### `Claims`

`Serialize, Deserialize, Debug, Clone`。字段：`sub: String`（用户 id）、`name: String`、`role: Role`、`iat: u64`、`exp: u64`。

### `Jwt`

无 derive（持有 jsonwebtoken 密钥，不可 Clone）。私有字段：`encoding: EncodingKey`、`decoding: DecodingKey`、`validation: Validation`、`ttl_secs: u64`。

- `pub const MIN_SECRET_LEN: usize = 32`
- `new(secret: &[u8], ttl_secs: u64) -> Result<Self, String>` — 密钥至少 32 字节，HS256 + 30s leeway。
- `issue(&self, user_id, name, role) -> Result<(String, u64), ApiError>` — 签发 token，返回 (token, exp)。
- `verify(&self, token: &str) -> Result<Claims, ApiError>` — 校验并解出 Claims。

自由函数：`hash_password(password)`（argon2id 哈希）；`verify_password(password, hash)`。

### `AuthUser(pub Claims)` / `AdminUser(pub Claims)`

均 `Debug, Clone`。实现 `FromRequestParts<AppState>`（`Rejection = ApiError`）：从 `Authorization: Bearer` 提取并校验 token；AdminUser 先走 AuthUser，再要求 `role == Admin`。

## 5.4 配置存储（`src/core/config_store.rs`）

### `ApplyResult`

`Serialize, Debug, PartialEq`。字段：`applied: bool`、`restart_required: Vec<&'static str>`、`hot_applied: Vec<&'static str>`。

### 自由函数（整个模块全是 free fn，无 impl struct）

- `public_json(config) -> Result<Json, ApiError>` — 脱敏 JSON（密钥字段 skip_serializing）。
- `etag(config) -> Result<String, ApiError>` — 脱敏 JSON 的 SHA-256 前 8 字节做 ETag。
- `public_toml(config) -> Result<String, ApiError>` — 脱敏 TOML。
- `preserve_secrets(new, old)`（私有）— 把旧配置明文密钥补回新配置。
- `changed<T: Serialize>(a, b)`（私有）— JSON 值比较。
- `classify(new, old)`（私有）— 划分需重启 / 热生效的配置节。
- `atomic_write(path, text)`（私有）— 临时文件 + rename 原子写，旧文件留 `.bak`。
- `apply(state, if_match, body) -> Result<ApplyResult, ApiError>` — 校验（If-Match、bind、模型引用、构造 agent 系统）→ 原子写盘 → 热替换 agents / memory_cfg / config。
- `unresolve_paths(config, base)`（私有）— `resolve_paths` 的逆操作。
- `restore_plain_secrets(text, config)`（私有）— TOML 写盘时把明文密钥键补回去。
- `read(state) -> Result<(String, Json), ApiError>` — GET /api/config 响应（ETag + 脱敏 JSON，展示相对路径）。
- `read_toml(state) -> Result<String, ApiError>` — `?format=toml` 版本。

## 5.5 用户与会话存储（`src/core/db.rs`）

### `Db`

derive `Clone`。私有字段：`db: Surreal<Any>`。

- `new(conn, namespace, database) -> Result<Self, surrealdb::Error>` — 切 ns/db 并建表（`user`、`chat_session` + 索引）。
- `query<T: DeserializeOwned>(&self, sql, vars)`（私有）— 通用查询，取最后一条语句结果。
- `user_count() -> Result<u64, ApiError>`
- `create_user(username, password, role) -> Result<UserView, ApiError>` — 校验用户名/密码长度，重名 409。
- `find_user(username) -> Result<Option<UserRow>, ApiError>`
- `list_users() -> Result<Vec<UserView>, ApiError>`
- `create_session(owner, title) -> Result<SessionView, ApiError>`
- `list_sessions(owner) -> Result<Vec<SessionView>, ApiError>`
- `get_session(owner, id) -> Result<(SessionView, Vec<Message>), ApiError>` — 校验归属，不存在即 404。
- `save_history(id, history: &[Message]) -> Result<(), ApiError>` — 覆盖保存会话历史。
- `delete_session(owner, id) -> Result<bool, ApiError>`

### `UserRow` / `UserView` / `SessionView`

- `UserRow { id, username, password_hash, role: Role }`（仅内部用，不落密到 API），`Deserialize, Debug, Clone`。
- `UserView { id, username, role }`，`Serialize, Deserialize, Debug, Clone, PartialEq`；`From<UserRow>`（剥离密码哈希）。
- `SessionView { id, title, created_at_ms: i64, updated_at_ms: i64 }`，同上 derive。

自由函数：`now_ms()`；`validate_username(name)`（3–32 位 `[A-Za-z0-9_.-]`）。`get_session` 内还有一个局部 `struct Row { #[serde(flatten)] view: SessionView, history: Vec<Message> }`。

## 5.6 错误（`src/core/error.rs`）

### `ApiError`

derive `Debug`。字段：`pub status: StatusCode`、`pub message: String`（对外消息；内部细节只进日志）。

- `new(status, message)` / `bad_request / unauthorized / forbidden / not_found / conflict(message)` — 各状态码快捷构造。
- `internal(detail)` — 500，详情写日志。
- `impl IntoResponse` — `{ "error": message }` JSON。
- `impl From<surrealdb::Error>`、`impl From<symbol::MemoryError>`（Invalid→400，其余→500）、`impl From<serde_json::Error>`。

## 5.7 共享状态（`src/core/state.rs`）

### `AppState`

derive `Clone`。字段：`inner: Arc<Inner>`、`jwt: Arc<Jwt>`（均 pub(crate)）。

方法：

- `new(config, config_path, db, tasks, agents, tools, memory, jwt) -> Self`（pub(crate)，`#[allow(clippy::too_many_arguments)]`）
- `config(&self) -> ServerConfig` — 当前配置快照（**对外公开**，thought-core 用它取 bind 地址）。
- `agents()` / `memory_cfg()` / `tasks()` / `db()`（均 pub(crate)）。
- `memory_for(user_id) -> Result<Option<MemoryStore>, ApiError>`（pub(crate)）— per_user 时返回 `user_<id>` scoped 的空间。
- `session(owner, id) -> Result<SessionHandle, ApiError>`（pub(crate)）— 取或恢复会话句柄（含注册记忆工具、DbSink、TaskRegistry observer、任务号续接），校验归属。
- `live_session(id) -> Option<SessionHandle>`（pub(crate)）— 不唤醒会话。
- `close_session(id)`（pub(crate)）— 丢弃句柄，主控循环退出、任务取消。

### `Inner`（pub(crate)）

字段：`db: Db`、`tasks: TaskRegistry`、`agents: RwLock<AgentSystem>`（可热替换）、`tools: Registry`（热替换时复用）、`config: RwLock<ServerConfig>`（含明文密钥）、`config_path: Option<PathBuf>`、`memory: Option<MemoryStore>`（记忆库根）、`memory_cfg: RwLock<Option<MemorySection>>`、`web_dir: PathBuf`、`sessions: Mutex<HashMap<String, LiveSession>>`（活跃会话）。

### `LiveSession`（pub(crate)）

字段：`owner: String`、`handle: SessionHandle`（来自 `agent` crate）。

### `DbSink(Db)`（私有）

`impl HistorySink`（async_trait）：`async save(session_id, history)` — 过滤 System 消息后写回 `chat_session.history`，失败仅 warn。

## 5.8 任务注册表（`src/core/tasks.rs`）

### 数据类型

- **`TaskStatus`**（enum）— `Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq`（snake_case）。变体：`Running / Done / Error / Cancelled`。
- **`TaskUsage`** — `Serialize, ..., Default, Eq`。字段：`prompt_tokens / completion_tokens / total_tokens: u64`。方法：`add(&mut self, u: &UsageView)`（私有，累加）。
- **`TaskView`** — `Serialize, Deserialize, Debug, Clone, PartialEq`。字段：`task_id: u64`、`session_id: String`、`worker: String`、`task: String`、`status: TaskStatus`、`started_at_ms: i64`、`finished_at_ms: Option<i64>`、`result: Option<String>`、`error: Option<String>`、`usage: TaskUsage`。
- **`TaskEvent`** — 同上 derive。字段：`ts_ms: i64`、`body: Json`（`#[serde(flatten)]`）。
- **`GlobalEvent`** — `Debug, Clone`。字段：`name: &'static str`、`data: Json`。
- **`Write`**（私有 enum）— 写库指令：`Upsert(TaskView, String)`、`Event { key: String, seq: u64, event: TaskEvent }`。
- **`Live`**（私有）— 运行中任务内存态：`view: TaskView`、`owner: String`、`seq: u64`、`pending_text: Option<(String, String, i64)>`（未落库的 text 段：agent、缓冲、开始时间）。

### `TaskRegistry`

derive `Clone`。字段：`inner: Arc<Inner>`（私有 Inner：`db: Surreal<Any>`、`writes: mpsc::UnboundedSender<Write>`、`live: Mutex<HashMap<String, Live>>`、`owners: Mutex<HashMap<String, String>>`、`global: broadcast::Sender<(String, GlobalEvent)>`）。

方法：

- `new(conn, namespace, database) -> Result<Self, surrealdb::Error>` — 建表（`task`、`task_event`），把上次 running 任务标记为 error，spawn 单写库后台任务，建 broadcast(2048)。
- `register_session(session_id, owner)` — 登记会话归属。
- `subscribe() -> broadcast::Receiver<(String, GlobalEvent)>` — 订阅全局事件流。
- `max_task_id(session_id) -> Result<u64, ApiError>` — 恢复会话时让任务号续增。
- `query<T: DeserializeOwned>(sql, vars)`（私有）。
- `list(owner, statuses, limit, cursor) -> Result<(Vec<TaskView>, Option<String>), ApiError>` — 游标分页（started_at_ms + key 倒序），运行中任务用量以内存为准。
- `detail(owner, session_id, task_id) -> Result<(TaskView, Vec<TaskEvent>), ApiError>` — 详情 + 事件，含未落库 text 段。
- `status(owner, session_id, task_id) -> Result<TaskStatus, ApiError>`。
- `write(w: Write)`（私有）— 投递写指令。
- `publish(owner, session_id, name, event)`（私有）— 扇出全局 broadcast。
- `flush_text(key, live)`（私有）— 把待落库 text 段写成 Event。
- `record(key, live, body)`（私有）— flush text 后记录一条事件。

### `impl EventObserver for TaskRegistry`

`observe(&self, session_id, event: &AgentEvent)` — 事件总入口（被 agent crate 以 `Arc<dyn EventObserver>` 回调）：

- `TaskStarted` — 建 Live + upsert + publish
- `TaskFinished` — 定状态/时间/结果 + upsert
- `TextDelta` — 按 agent 标签合并 text 段
- `ToolCall` / `ToolResult` — record + publish
- `Usage` — 累加 + record + upsert 累计用量
- `ReasoningDelta` / `Reply` / `Error` — 忽略，不属任务活动

### 自由函数

`now_ms()`；`task_key(session_id, task_id) -> String`（`"{sid}_{tid}"`）；`task_of(agent) -> Option<u64>`（解析 `coder#3[/sub]` 标签）；`clip(text)`（16KiB 截断）；`encode_cursor / decode_cursor`（十六进制不透明游标）。

## 5.9 路由（`src/routes/*`，handler 均为 free fn，`pub(super)`）

路由表由 `router(state: AppState) -> Router` 组装（`routes/mod.rs`），含 `/health`、全套 `/api/*`，兜底 `api_not_found` 返回 JSON 404；`web::security_headers` 中间件加 CSP/nosniff 等安全头。

### admin.rs

- handler：`list_users`、`create_user`、`list_models`、`list_agents`、`get_config`（支持 `?format=toml`）、`put_config`（If-Match 校验）。
- 请求体：`CreateUserRequest { username, password, role }`（role 默认 User）、`ConfigQuery { format }`。

### auth.rs

- handler：`login`（用户不存在用 dummy 哈希防时序探测）、`me`。
- 请求/响应体：`LoginRequest`、`LoginResponse { token, expires_at, user }`。

### memory.rs

- handler：`search_memory`（`SearchQuery { q, limit≤50 }`）、`recent_memory`（`RecentQuery { limit≤200 }`）、`save_memory`（`SaveMemoryRequest { content, tags, external_id }`，超 64KiB 拒绝）、`delete_memory`。
- 私有 helper：`memory(state, user_id) -> Result<MemoryStore, ApiError>`。

### sessions.rs

- handler：`list_sessions`、`create_session`（`CreateSessionRequest { title }`，默认 "New chat"，截 200 字符）、`get_session`（`SessionDetail`，flatten SessionView + history）、`delete_session`、`send_message`（`SendMessageRequest { content }`，202；可选自动写入记忆）、`session_events`（SSE，`event_name` 把 `AgentEvent` 映射为事件名，lagged 事件告知丢帧）、`session_tasks`。
- 私有 `fn event_name(&AgentEvent) -> &'static str`。

### tasks.rs

- handler：`list_tasks`（`ListQuery { status, limit, cursor }` → `ListResponse { tasks, next_cursor }`）、`get_task`（`DetailResponse`，flatten TaskView + events）、`cancel_task`（仅 running 可取消，不唤醒会话）、`task_events`（全局 SSE，按 owner 过滤）。

### web.rs

- `router(web_dir) -> Router`（ServeDir + index.html SPA 兜底）。
- `security_headers(request, next) -> Response` — 加 CSP/nosniff 等安全头。

## 5.10 组合关系

- `Built` → `Router` + `AppState`。
- `AppState` → `Arc<Inner>` + `Arc<Jwt>`；`Inner` 持有：`Db`（用户/会话）、`TaskRegistry`、`RwLock<AgentSystem>`、`Registry`（工具池）、`RwLock<ServerConfig>`、`Option<MemoryStore>`（记忆根）+ `RwLock<Option<MemorySection>>`、`Mutex<HashMap<String, LiveSession>>`、`web_dir`。
- `LiveSession` → `SessionHandle`（agent crate）；会话的 `SessionOptions` 又注入 `DbSink(Db)` 和 `TaskRegistry.clone()` 作为 observer/sink → 回环持有关系。
- `MemoryStore`（symbol）被 `AppState.Inner` 持有根实例；`per_user` 时 `memory_for` 派生 scoped clone 给路由层。
- `TaskRegistry.Inner` → `Surreal<Any>` + 写队列 + `Live` 内存表（`Live` 内含 `TaskView`）。
- 配置侧：`ServerConfig` 聚合 `HttpConfig` / `AuthConfig` / `StorageConfig` / `EmbeddingConfig` / `MemorySection`（flatten `MemoryConfig`）+ 外部 crate 的 `ModelConfig` / `AgentConfig`。

## 5.11 对外 API 面

- `pub mod config; pub mod core; pub mod routes;`（core 整体公开，但多数类型/方法是 `pub(crate)`）。
- `pub use config::ServerConfig`、`pub use core::state::AppState`。
- 自由函数：`build`、`build_from_file`、`spawn_memory_worker`、`maintain_spaces`（+ `BuildError`、`Built` 为 pub）。

`thought-core/src/main.rs` 实际只用到：`build_from_file`、`spawn_memory_worker`、`built.state.config()`（即 `AppState::config`）、`ServerConfig`。

---

# 6. `thought-core` crate — 二进制入口

唯一文件 `src/main.rs`（67 行）。`[[bin]] name = "thought"`，无 `lib.rs`，不产出 rlib，无公开 API 面。

## 6.1 数据结构

**无任何 struct / enum / trait**。唯一数据项是模块级常量：

- `const MEMORY_MAINTENANCE_INTERVAL: Duration`（`main.rs:16`）— 记忆维护（抽取队列 + 衰减）的间隔，固定 60 秒。

## 6.2 函数

- `fn main() -> ExitCode`（`main.rs:19`）— `#[tokio::main]` 入口；初始化 `tracing_subscriber`（EnvFilter，默认 `info` 级别），调用 `run()`，成功返回 `ExitCode::SUCCESS`，失败记日志并返回 `ExitCode::FAILURE`。
- `async fn run() -> Result<(), String>`（私有）— 组装并启动系统：
  1. 取命令行第一个参数作为配置文件路径（默认 `./thought.toml`）；
  2. `plugin::tools::file_tools::builtin_registry()` 构造内置工具池；
  3. `server::build_from_file(path, tools)` 从文件组装整个系统（相对路径以配置文件所在目录为准）；
  4. 若配置启用了 memory，则 `server::spawn_memory_worker(state, 60s)` 启动记忆维护后台任务；
  5. 按 `config.http.bind` 绑定 `TcpListener`，用 `axum::serve` 启动 HTTP 服务，`ctrl_c` 优雅退出。
- `mod tests` 内 `fn example_config_parses()`（测试）— 解析仓库根的 `thought.example.toml` 为 `ServerConfig`，断言 `models` 含 `deepseek`、`agent.workers[0].name == "coder"`。

CLI 接口：`thought [CONFIG]`，默认读取 `./thought.toml`。

---

# 附：crate 间调用关系速查

| 调用方 | 被调方 | 用到的关键项 |
|---|---|---|
| thought-core → plugin | `builtin_registry()` | 工具池 |
| thought-core → server | `build_from_file`、`spawn_memory_worker`、`AppState::config`、`ServerConfig` | 组装与启动 |
| server → agent | `AgentSystem::new/start_session`、`SessionHandle::{send_user,subscribe,tasks,cancel_task}`、`AgentConfig`、`HistorySink`、`SessionOptions`、`EventObserver`、`UsageView`、`AgentEvent` | agent 系统 |
| server → model | `ModelConfig`、`EmbeddingModel` | 模型池与向量化配置 |
| server → symbol | `MemoryStore`、`MemorySection(MemoryConfig)`、`register_memory_tools`、`MemoryError` | 记忆 |
| server → plugin | `Registry`、`Tool` | 工具池 |
| agent → model | `Model`、`Message`、`Usage`、`ChatChunk`、`ToolCall`、`Function` | 模型调用 |
| agent → plugin | `Registry`、`Tool` | 工具注册与分发 |
| symbol → model | `EmbeddingModel`、`Model` | 嵌入与抽取 |
| symbol → plugin | `Registry`、`Tool` | 记忆工具暴露 |
