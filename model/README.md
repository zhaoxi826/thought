# model

大模型接口适配层。把不同厂商的聊天接口统一到一个 trait 之下，屏蔽协议差异。

## 设计：两层枚举

接口本质上只有**两种协议**，厂商差异分两层处理：

- **第一层（协议 / 客户端）**——OpenAI Chat Completions 与 Anthropic Messages，两者
  端点、鉴权、请求/响应结构差异大，客户端彻底分开。对应顶层 [`Model`] 枚举。
- **第二层（vendor / 构造请求时）**——同一协议下厂商只差几个字段。OpenAI 协议下用
  [`OpenAiVendor`] 枚举承载：`OpenAi` 变体带 `frequency_penalty` / `presence_penalty` /
  `tool_choice`，`Deepseek` 变体带 `thinking` / `reasoning_effort`。借助
  `#[serde(untagged)] + #[serde(flatten)]`，选定 vendor 后其字段直接平铺进请求体顶层，
  从类型上杜绝“给 OpenAI 塞 thinking”这类非法组合。

因此对外只有三个配置：`openai::openai`、`openai::deepseek`、`anthropic::anthropic`。
DeepSeek 不是独立类型，只是 OpenAI 客户端的一组预设。

## 模块结构

```
src/model/
  base.rs            跨协议共享：Message / Model trait、Error、消息类型、percent
  openai.rs          openai 包根（模块声明 + 重导出）
  openai/
    openai.rs        OpenAI 协议引擎：客户端 + 线协议 + OpenAiArgs / OpenAiVendor + 原生预设
    deepseek.rs      DeepSeek 预设（薄层，单向依赖 openai.rs）
  anthropic.rs       anthropic 包根
  anthropic/
    anthropic.rs     AnthropicModel / AnthropicArgs + 其线协议
```

## 快速开始

```rust
use model::{Model, OpenAiModel, OpenAiArgs, AnthropicModel, AnthropicArgs, OpenAIMessage, Message};

# async fn demo() -> Result<(), model::Error> {
// 原生 OpenAI
let gpt = OpenAiModel::new("", "sk-...", OpenAiArgs::openai("gpt-5"));

// DeepSeek（OpenAI 兼容，默认开思考模式）
let ds = OpenAiModel::deepseek("", "sk-...", OpenAiArgs::deepseek("deepseek-v4-pro"));

// Anthropic
let claude = AnthropicModel::new("", "sk-ant-...", AnthropicArgs::new("claude-opus-4-8"));

// 统一入口：Model 枚举，收中立的 {role, content}
let m = Model::OpenAi(ds);
let messages = vec![
    OpenAIMessage::new("system", "You are a helpful assistant."),
    OpenAIMessage::new("user", "Hi"),
];
let output = m.chat(messages, None, None).await?;
println!("{:?}", output.content);
# Ok(())
# }
```

## API

### 顶层：`Model` 枚举

协议无关的运行时封装，`chat` 以中立的 `{role, content}`（`OpenAIMessage`）收参，
Anthropic 分支内部转成 content block。

| 方法 | 说明 |
| --- | --- |
| `Model::OpenAi(OpenAiModel)` / `Model::Anthropic(AnthropicModel)` | 两个协议 arm |
| `async chat(messages: Vec<OpenAIMessage>, user_id: Option<&str>, response_format: Option<&str>) -> Result<ChatOutput, Error>` | 非流式对话，返回结构化结果（文本 + 工具调用 + 结束原因 + 用量） |
| `async chat_stream(...) -> Result<ChatStream, Error>` | 流式对话，返回逐帧增量的 chunk 流 |
| `async get_models(&mut self) -> Result<(), Error>` | 拉取模型列表写入 `model_list` |
| `set_tools(Vec<Function>)` | 覆盖可用工具列表，构造请求时各协议自行翻译成 wire 格式 |

### 统一 trait：`ModelTrait`（`base::Model`）

各客户端实现的泛型接口，用关联类型区分消息格式。

- `type Message: Message` —— OpenAI 侧为 `OpenAIMessage`，Anthropic 侧为 `AnthropicMessage`
- `async chat(messages: Vec<Self::Message>, user_id, response_format) -> Result<ChatOutput, Error>`
- `async chat_stream(messages: Vec<Self::Message>, user_id, response_format) -> Result<ChatStream, Error>`
- `async get_models(&mut self) -> Result<(), Error>`

### 结构化输出与流式（`base`）

- **`ChatOutput`** `{ content: Option<String>, reasoning: Option<String>, tool_calls: Vec<ToolCall>,
  finish_reason: String, usage: Usage }` —— 非流式结果。模型只回工具调用时 `content` 为 `None`；
  `finish_reason` 为协议原生字符串（OpenAI `stop` / `tool_calls`，Anthropic `end_turn` /
  `tool_use` / `max_tokens`）。
- **`ChatStream`** = `BoxStream<'static, Result<ChatChunk, Error>>` —— 流式结果。
  **`ChatChunk`** 的每个字段都是增量：`content` / `reasoning` 为文本碎片，
  `tool_calls: Vec<ToolCallDelta>` 为工具调用碎片（`id` / `name` 只在首个片段出现，
  `arguments` 需按 `index` 自行拼接），`finish_reason` 只在最后一帧出现。
- **`ToolCall`** `{ id, name, arguments }` —— 一次完整工具调用，`arguments` 为原始 JSON 参数串。
- **`Usage`** `{ prompt_tokens, completion_tokens, total_tokens }` —— Anthropic 的
  input/output 映射为 prompt/completion。

### OpenAI 协议（DeepSeek 为预设）

**`OpenAiModel`**

| 构造 | 默认 base URL |
| --- | --- |
| `OpenAiModel::new(base_url, api_key, args)` | `OPENAI_BASE_URL`（`base_url` 为空时） |
| `OpenAiModel::deepseek(base_url, api_key, args)` | `DEEPSEEK_BASE_URL`（`base_url` 为空时） |

公开字段：`pub model_list: Vec<String>`、`pub args: OpenAiArgs`。

**`OpenAiArgs`**（builder 风格，全部返回 `Self`）

| 方法 | 适用 vendor |
| --- | --- |
| `OpenAiArgs::openai(model)` / `OpenAiArgs::deepseek(model)` | 构造 |
| `.temperature(u8)` `.top_p(u8)` `.tools(Vec<Tool>)` | 通用 |
| `.tool_choice(ToolChoice)` `.penalties(freq, presence)` | 仅 OpenAI |
| `.thinking(Thinking, ThinkingConfig)` | 仅 DeepSeek |

> 注：setter 与当前 vendor 不匹配时是**空操作**（例如 OpenAI 预设调 `.thinking(...)` 会被忽略），不会报错。

相关类型：`OpenAiVendor`、`Thinking` / `ThinkingType` / `ThinkingConfig`、`ToolChoice`、
`Tool` / `Function` / `Parameter` / `ParameterType`、`ResponseFormat`、`ChatRequest` / `ChatResponse`。
常量：`OPENAI_BASE_URL`、`DEEPSEEK_BASE_URL`。

### Anthropic 协议

**`AnthropicModel`**

- `AnthropicModel::new(base_url, api_key, args)` —— `base_url` 为空时回退 `ANTHROPIC_BASE_URL`
- `.version(impl Into<String>)` —— 覆盖 `anthropic-version` 头（默认 `DEFAULT_VERSION`）
- 走 `/v1/messages`，鉴权用 `x-api-key` + `anthropic-version` 头
- `chat` 会把 `role == "system"` 的消息抽取合并进顶层 `system` 字段

**`AnthropicArgs`**（builder 风格）

- `AnthropicArgs::new(model)`
- `.max_tokens(u32)` `.system(impl Into<String>)` `.temperature(u8)` `.top_p(u8)`

常量：`ANTHROPIC_BASE_URL`、`DEFAULT_VERSION`、`DEFAULT_MAX_TOKENS`。

### 跨协议公共件（`base`）

- **`Error`** —— `Http(reqwest::Error)` / `EmptyResponse` / `Protocol(String)`（流式事件体等
  无法按协议解析）；实现 `Display` / `std::error::Error` / `From<reqwest::Error>`。
  响应无内容时返回 `EmptyResponse` 而非静默空串。
- **`Message` trait** —— `role()` / `content()` / `new(role, content)`
- **`OpenAIMessage`** `{ role, content }`、**`AnthropicMessage`** `{ role, content: Vec<block> }`
  （含 `AnthropicContentBlock`）
- **`percent`** —— serde 辅助模块

## 关于 temperature / top_p 的 u8 表示

`temperature` / `top_p` 内部用 **u8 的“百分之一”定点数**表示：`70` 即 `0.7`、`100` 即
`1.0`、`150` 即 `1.5`。序列化时由 `percent` 模块用整数运算拼出十进制字符串再作为 JSON
number 输出，**全程不经过 f32/f64**，既无浮点误差也无精度损失。这是刻意设计，避免浮点
的体积、速度与舍入问题。

## 现状与限制

- **消息模型还是纯文本**：`OpenAIMessage` / `AnthropicMessage` 只有 `{role, content}`，
  工具结果回传（`tool` 角色 / `tool_result` 块）与 assistant 的 `tool_calls` 回放进消息
  尚未支持——工具调用目前只能通过 `ChatOutput::tool_calls` 拿到，多轮工具循环的消息
  模型待补。
- **流式不回传用量**：SSE chunk 里的 usage（OpenAI `stream_options`、Anthropic
  `message_delta.usage`）暂未解析进 `ChatChunk`。
- **Anthropic 无 `response_format`**：该参数在 Anthropic 分支被忽略（结构化输出走工具）。
- **DeepSeek 的 Anthropic 兼容**尚未实现；届时在 `anthropic` 包内对称地加 `deepseek.rs`
  + 第二层 `AnthropicVendor` 即可，结构已预留。

## 测试

离线测试覆盖全部 wire format 与流式解析，不需要 API key：

```bash
cargo test -p model
```

- `src` 内单元测试：`ToolChoice` / `ResponseFormat` / `percent` 序列化、请求体快照
  （vendor 字段平铺、system 提取）、响应与流式事件映射、SSE 解析器。
- `tests/mock_http.rs`：用本地迷你 TCP server 跑通 chat / chat_stream 全链路
  （请求构造 → 发送 → 解析），两个协议各一组。

`tests/deepseek_integration.rs` 为联网集成测试（标记 `#[ignore]`，默认不跑），
需设置环境变量 `API_KEY`（形如 `sk-...`）：

```bash
API_KEY=sk-xxx cargo test -p model --test deepseek_integration -- --ignored
```
