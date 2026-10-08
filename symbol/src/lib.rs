//! symbol：符号侧工具。目前提供持久化记忆器。
//!
//! 设计参考 A_memorix 的 MVP 子集，存储在 SurrealDB（内嵌 SurrealKV）：
//! 段落（记忆主体）+ 实体 / 关系图（LLM 异步抽取）+ 三路检索融合 + 关系半衰期衰减。
//! 入口是 [`MemoryStore`]；[`register_memory_tools`] 把它包成 agent 工具。

pub mod memory;

pub use memory::text;

pub use memory::config::{MemoryConfig, RetrievalConfig};
pub use memory::embed::{ApiEmbedder, Embedder};
pub use memory::error::MemoryError;
pub use memory::extract::{
    parse_extraction, ExtractedEntity, ExtractedRelation, Extraction, Extractor, LlmExtractor,
};
pub use memory::store::MemoryStore;
pub use memory::tools::register_memory_tools;
pub use memory::types::{
    ChannelScores, IngestRequest, IngestResult, Memory, MemoryHit, MemoryStats, RelationView,
};
