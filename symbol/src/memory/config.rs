//! 记忆器配置。

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct MemoryConfig {
    /// SurrealDB 地址：`surrealkv://data/memory`（内嵌持久化）或 `mem://`（测试）。
    pub endpoint: String,

    /// SurrealDB namespace；database 即记忆空间（见 `MemoryStore::scoped`）。
    pub namespace: String,
    pub database: String,

    /// 向量维度，须与 embedding 模型输出一致。
    pub embedding_dimension: usize,

    /// 关系强度半衰期（小时）。
    pub half_life_hours: f64,

    /// 有效强度低于此值的关系不再参与检索（冻结），回升后自动复活。
    pub inactive_threshold: f64,

    /// `prune` 时删除有效强度低于此值的关系。
    pub prune_threshold: f64,

    /// 抽取结果中低于此置信度的关系丢弃。
    pub min_confidence: f64,

    pub retrieval: RetrievalConfig,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            endpoint: "surrealkv://data/memory".to_owned(),
            namespace: "thought".to_owned(),
            database: "default".to_owned(),
            embedding_dimension: 1024,
            half_life_hours: 24.0 * 7.0,
            inactive_threshold: 0.1,
            prune_threshold: 0.05,
            min_confidence: 0.5,
            retrieval: RetrievalConfig::default(),
        }
    }
}

/// 三路检索融合参数。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct RetrievalConfig {
    /// 各通道候选数。
    pub semantic_top_k: usize,
    pub sparse_top_k: usize,
    pub graph_top_k: usize,

    /// 融合权重；某通道不可用（如未配置 embedding）时其余按比例放大。
    pub semantic_weight: f64,
    pub sparse_weight: f64,
    pub graph_weight: f64,

    /// 绝对下限与相对首条的比例下限，两者取大。
    pub min_score: f64,
    pub relative_cutoff: f64,

    /// 同一关系两次“访问强化”的最小间隔（毫秒）。
    pub access_cooldown_ms: i64,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            semantic_top_k: 20,
            sparse_top_k: 20,
            graph_top_k: 40,
            semantic_weight: 0.65,
            sparse_weight: 0.20,
            graph_weight: 0.15,
            min_score: 0.15,
            relative_cutoff: 0.35,
            access_cooldown_ms: 3_600_000,
        }
    }
}
