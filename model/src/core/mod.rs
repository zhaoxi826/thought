//! 协议无关的公共件：消息、输出、流式累加、工具定义、错误。

pub mod error;
pub mod message;
pub mod output;
pub mod percent;
pub mod stream;
pub mod tool;

pub use error::Error;
pub(crate) use error::check_status;
pub use message::{Message, Model};
pub use output::{ChatChunk, ChatOutput, ToolCall, ToolCallDelta, Usage};
pub use stream::{collect_stream, ChatStream, StreamAccumulator};
pub use tool::{Function, Parameter, ParameterType};
