//! 记忆存储的数据类型与 SurrealDB schema。

use serde::{Deserialize, Serialize};

/// 每个记忆空间（SurrealDB database）初始化时执行的 schema，幂等。
///
/// - `paragraph`：记忆主体，id 为规范化内容 hash；`terms` 为 Rust 侧分好的检索词
///   （中文二元组），用 `blank` 分词器建 BM25；`embedding` 建 HNSW。
/// - `entity`：实体，id 为规范化名字 hash。
/// - `relation`：`entity -> relation -> entity` 图边，id 为 (s,p,o) hash；`paragraphs`
///   记录支撑它的段落，强度按半衰期惰性衰减（`strength` + `anchor_ms`）。
pub(crate) fn schema(dimension: usize) -> String {
    format!(
        "
DEFINE ANALYZER IF NOT EXISTS memory_terms TOKENIZERS blank FILTERS lowercase;

DEFINE TABLE IF NOT EXISTS paragraph SCHEMALESS;
DEFINE INDEX IF NOT EXISTS paragraph_terms ON paragraph FIELDS terms FULLTEXT ANALYZER memory_terms BM25;
DEFINE INDEX IF NOT EXISTS paragraph_vec ON paragraph FIELDS embedding HNSW DIMENSION {dimension} DIST COSINE;
DEFINE INDEX IF NOT EXISTS paragraph_external ON paragraph FIELDS external_id;
DEFINE INDEX IF NOT EXISTS paragraph_source ON paragraph FIELDS source;
DEFINE INDEX IF NOT EXISTS paragraph_pending ON paragraph FIELDS extracted;

DEFINE TABLE IF NOT EXISTS entity SCHEMALESS;

DEFINE TABLE IF NOT EXISTS relation TYPE RELATION IN entity OUT entity SCHEMALESS;
DEFINE INDEX IF NOT EXISTS relation_terms ON relation FIELDS terms FULLTEXT ANALYZER memory_terms BM25;
DEFINE INDEX IF NOT EXISTS relation_vec ON relation FIELDS embedding HNSW DIMENSION {dimension} DIST COSINE;
"
    )
}

/// 写入一条记忆的请求。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct IngestRequest {
    pub content: String,

    /// 来源标签，如 `chat:<session>`、`agent:coder`，可按来源批量删除。
    #[serde(default)]
    pub source: String,

    /// 调用方的幂等键：同一 `external_id` 只写入一次。
    #[serde(default)]
    pub external_id: Option<String>,

    #[serde(default)]
    pub tags: Vec<String>,

    /// 是否排队做三元组抽取（需要配置抽取器）。
    #[serde(default = "default_true")]
    pub extract: bool,
}

fn default_true() -> bool {
    true
}

impl IngestRequest {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            extract: true,
            ..Default::default()
        }
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn external_id(mut self, id: impl Into<String>) -> Self {
        self.external_id = Some(id.into());
        self
    }

    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// 写入结果。
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct IngestResult {
    /// 段落 id（内容 hash）。
    pub id: String,

    /// `false` 表示内容或 `external_id` 已存在，只合并了标签。
    pub created: bool,
}

/// 一条检索命中。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MemoryHit {
    pub id: String,
    pub content: String,
    pub source: String,
    pub tags: Vec<String>,
    pub created_at_ms: i64,

    /// 融合后的分数（0–1）。
    pub score: f64,

    /// 各通道分数，便于调参排查。
    pub channels: ChannelScores,

    /// 命中的支撑关系，形如 `alice -likes-> rust`。
    pub relations: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct ChannelScores {
    pub semantic: f64,
    pub sparse: f64,
    pub graph: f64,
}

/// 一条记忆（段落）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: String,
    pub content: String,
    pub source: String,
    pub tags: Vec<String>,
    pub created_at_ms: i64,
}

impl From<ParagraphRow> for Memory {
    fn from(r: ParagraphRow) -> Self {
        Self {
            id: r.id,
            content: r.content,
            source: r.source,
            tags: r.tags,
            created_at_ms: r.created_at_ms,
        }
    }
}

/// 计数概况。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub paragraphs: u64,
    pub pending_extraction: u64,
    pub entities: u64,
    pub relations: u64,
    pub inactive_relations: u64,
}

/// 存储里的段落行（检索 / 读取用）。
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct ParagraphRow {
    pub id: String,
    pub content: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at_ms: i64,
}

/// 存储里的关系行。
#[derive(Deserialize, Debug, Clone)]
pub(crate) struct RelationRow {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    #[serde(default)]
    pub paragraphs: Vec<String>,
    #[serde(default = "one")]
    pub strength: f64,
    #[serde(default)]
    pub anchor_ms: i64,
    #[serde(default)]
    pub pinned: bool,
}

fn one() -> f64 {
    1.0
}

/// 关系的对外视图。
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct RelationView {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    /// 当前有效强度（已衰减）。
    pub strength: f64,
    pub paragraphs: Vec<String>,
}

/// 半衰期衰减：`strength × 2^(-Δh / half_life)`；钉住的关系不衰减。
pub(crate) fn effective_strength(row: &RelationRow, now_ms: i64, half_life_hours: f64) -> f64 {
    if row.pinned {
        return row.strength;
    }
    let hours = (now_ms - row.anchor_ms).max(0) as f64 / 3_600_000.0;
    row.strength * 0.5f64.powf(hours / half_life_hours)
}
