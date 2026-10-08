//! symbol：符号侧工具。目前提供持久化记忆器。
//!
//! 设计参考 A_memorix 的 MVP 子集，存储在 SurrealDB（内嵌 SurrealKV）：
//! 段落（记忆主体）+ 实体 / 关系图（LLM 异步抽取）+ 三路检索融合 + 关系半衰期衰减。
//! 入口是 [`MemoryStore`]；[`register_memory_tools`] 把它包成 agent 工具。

mod config;
mod embed;
mod error;
mod extract;
mod retrieve;
mod store;
pub mod text;
mod tools;
mod types;

pub use config::{MemoryConfig, RetrievalConfig};
pub use embed::{ApiEmbedder, Embedder};
pub use error::MemoryError;
pub use extract::{parse_extraction, ExtractedEntity, ExtractedRelation, Extraction, Extractor, LlmExtractor};
pub use store::MemoryStore;
pub use tools::register_memory_tools;
pub use types::{
    ChannelScores, IngestRequest, IngestResult, Memory, MemoryHit, MemoryStats, RelationView,
};
