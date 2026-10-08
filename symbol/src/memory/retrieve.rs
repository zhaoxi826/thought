//! 三路检索融合与关系生命周期。
//!
//! 通道（对应 A_memorix 的 dual-path 检索的 MVP 子集）：
//! 1. **semantic**：段落向量 KNN，分数 = 1 − 余弦距离；
//! 2. **sparse**：段落 BM25（中文二元组），按本次最高分归一化；
//! 3. **graph**：关系向量 KNN + 关系 BM25，命中关系按有效强度加权后展开到支撑段落，
//!    段落图分 = 最强关系分 + 0.35 × 其余关系分之和（封顶 1）。
//!
//! 融合：加权求和（不可用的通道权重按比例让给其余通道），再用
//! `max(min_score, 首条 × relative_cutoff)` 截断。最终返回的命中所支撑的关系会被
//! “访问强化”（有冷却），常被用到的记忆衰减更慢。

use crate::memory::store::MemoryStore;
use crate::memory::text::query_terms;
use crate::memory::types::*;
use crate::memory::error::MemoryError;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Scored {
    id: String,
    score: f64,
}

#[derive(Deserialize)]
struct ScoredRelation {
    #[serde(flatten)]
    row: RelationRow,
    score: f64,
}

/// 命中关系展开到段落时，每条关系最多带几段。
const PARAGRAPHS_PER_RELATION: usize = 5;

/// 次要关系对段落图分的贡献系数。
const SECONDARY_RELATION_WEIGHT: f64 = 0.35;

/// 访问强化：`s += ACCESS_GAIN × (1 − s)`。
const ACCESS_GAIN: f64 = 0.05;

impl MemoryStore {
    /// 检索与 `query` 相关的记忆，按融合分数降序返回至多 `limit` 条。
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryHit>, MemoryError> {
        let terms = query_terms(query);
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let cfg = &self.config().retrieval;
        let now = now_ms();
        let embedding = self.embed_query(query).await?;

        // ---- semantic ----
        let mut semantic: HashMap<String, f64> = HashMap::new();
        if let Some(q) = &embedding {
            let rows: Vec<Scored> = self
                .query_rows(
                    &format!(
                        "SELECT meta::id(id) AS id, 1 - vector::distance::knn() AS score FROM paragraph
                         WHERE embedding <|{k},{ef}|> $q",
                        k = cfg.semantic_top_k,
                        ef = cfg.semantic_top_k.max(40)
                    ),
                    json!({ "q": q }),
                )
                .await?;
            semantic.extend(rows.into_iter().map(|r| (r.id, r.score.clamp(0.0, 1.0))));
        }

        // ---- sparse ----
        let rows: Vec<Scored> = self
            .query_rows(
                "SELECT meta::id(id) AS id, search::score(1) AS score FROM paragraph
                 WHERE terms @1,OR@ $terms ORDER BY score DESC LIMIT $k",
                json!({ "terms": terms, "k": cfg.sparse_top_k }),
            )
            .await?;
        let sparse = normalize_by_max(rows);

        // ---- graph ----
        let mut relations: HashMap<String, (RelationRow, f64)> = HashMap::new();
        let mut add_relations = |rows: Vec<ScoredRelation>, weight: f64| {
            for r in rows {
                let score = r.score * weight;
                let entry = relations.entry(r.row.id.clone()).or_insert((r.row, 0.0));
                entry.1 = entry.1.max(score);
            }
        };
        const RELATION_FIELDS: &str = "meta::id(id) AS id, subject, predicate, object, paragraphs,
            strength, anchor_ms, pinned";
        if let Some(q) = &embedding {
            let rows: Vec<ScoredRelation> = self
                .query_rows(
                    &format!(
                        "SELECT {RELATION_FIELDS}, 1 - vector::distance::knn() AS score FROM relation
                         WHERE embedding <|{k},{ef}|> $q AND inactive = false",
                        k = cfg.graph_top_k,
                        ef = cfg.graph_top_k.max(40)
                    ),
                    json!({ "q": q }),
                )
                .await?;
            add_relations(rows, 1.0);
        }
        let rows: Vec<ScoredRelation> = self
            .query_rows(
                &format!(
                    "SELECT {RELATION_FIELDS}, search::score(1) AS score FROM relation
                     WHERE terms @1,OR@ $terms AND inactive = false ORDER BY score DESC LIMIT $k"
                ),
                json!({ "terms": terms, "k": cfg.graph_top_k }),
            )
            .await?;
        let max = rows.iter().map(|r| r.score).fold(0.0, f64::max);
        let rows = rows
            .into_iter()
            .map(|mut r| {
                r.score = if max > 0.0 { r.score / max } else { 1.0 };
                r
            })
            .collect();
        // 关键词命中关系的可信度低于语义命中，打个折
        add_relations(rows, 0.8);

        let half_life = self.config().half_life_hours;
        let mut per_paragraph: HashMap<String, Vec<(f64, String, String)>> = HashMap::new();
        for (row, score) in relations.values() {
            let strength = effective_strength(row, now, half_life);
            if strength < self.config().inactive_threshold {
                continue;
            }
            let weighted = score * strength.min(1.0);
            let label = format!("{} -{}-> {}", row.subject, row.predicate, row.object);
            for p in row.paragraphs.iter().rev().take(PARAGRAPHS_PER_RELATION) {
                per_paragraph
                    .entry(p.clone())
                    .or_default()
                    .push((weighted, row.id.clone(), label.clone()));
            }
        }
        let mut graph: HashMap<String, (f64, Vec<String>, Vec<String>)> = HashMap::new();
        for (paragraph, mut hits) in per_paragraph {
            hits.sort_by(|a, b| b.0.total_cmp(&a.0));
            let score = (hits[0].0
                + SECONDARY_RELATION_WEIGHT * hits[1..].iter().map(|h| h.0).sum::<f64>())
            .min(1.0);
            let ids = hits.iter().map(|h| h.1.clone()).collect();
            let labels = hits.into_iter().map(|h| h.2).collect();
            graph.insert(paragraph, (score, ids, labels));
        }

        // ---- 融合 ----
        let mut weights = [cfg.semantic_weight, cfg.sparse_weight, cfg.graph_weight];
        if embedding.is_none() {
            weights[0] = 0.0;
        }
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            return Ok(Vec::new());
        }
        let weights = weights.map(|w| w / total);

        let mut candidates: HashMap<String, ChannelScores> = HashMap::new();
        for (id, s) in &semantic {
            candidates.entry(id.clone()).or_default().semantic = *s;
        }
        for (id, s) in &sparse {
            candidates.entry(id.clone()).or_default().sparse = *s;
        }
        for (id, (s, _, _)) in &graph {
            candidates.entry(id.clone()).or_default().graph = *s;
        }
        let mut ranked: Vec<(String, ChannelScores, f64)> = candidates
            .into_iter()
            .map(|(id, c)| {
                let score = weights[0] * c.semantic + weights[1] * c.sparse + weights[2] * c.graph;
                (id, c, score)
            })
            .collect();
        ranked.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

        let cutoff = ranked
            .first()
            .map_or(0.0, |top| (top.2 * cfg.relative_cutoff).max(cfg.min_score));
        ranked.retain(|r| r.2 >= cutoff);
        ranked.truncate(limit);
        if ranked.is_empty() {
            return Ok(Vec::new());
        }

        // ---- 取段落正文 ----
        let ids: Vec<&String> = ranked.iter().map(|r| &r.0).collect();
        let rows: Vec<ParagraphRow> = self
            .query_rows(
                "SELECT meta::id(id) AS id, content, source, tags, created_at_ms FROM paragraph
                 WHERE meta::id(id) IN $ids",
                json!({ "ids": ids }),
            )
            .await?;
        let mut rows: HashMap<String, ParagraphRow> = rows.into_iter().map(|r| (r.id.clone(), r)).collect();

        let mut touched = Vec::new();
        let hits = ranked
            .into_iter()
            .filter_map(|(id, channels, score)| {
                let row = rows.remove(&id)?;
                let (relation_ids, labels) = graph
                    .remove(&id)
                    .map(|(_, ids, labels)| (ids, labels))
                    .unwrap_or_default();
                touched.extend(relation_ids);
                Some(MemoryHit {
                    id,
                    content: row.content,
                    source: row.source,
                    tags: row.tags,
                    created_at_ms: row.created_at_ms,
                    score,
                    channels,
                    relations: labels,
                })
            })
            .collect();

        touched.sort();
        touched.dedup();
        self.reinforce_access(&touched, now).await?;
        Ok(hits)
    }

    /// 对一组关系做“访问强化”：先把衰减结算进 `strength`，再按冷却期增益。
    async fn reinforce_access(&self, ids: &[String], now: i64) -> Result<(), MemoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let half_life = self.config().half_life_hours;
        self.execute_vars(
            "UPDATE relation SET
                strength = math::min([1.0, strength * math::pow(0.5, (($now - anchor_ms) / 3600000.0) / $half_life)
                    * (1 - $gain) + $gain]),
                anchor_ms = $now,
                last_access_ms = $now
             WHERE meta::id(id) IN $ids AND pinned = false AND $now - last_access_ms >= $cooldown",
            json!({
                "ids": ids,
                "now": now,
                "half_life": half_life,
                "gain": ACCESS_GAIN,
                "cooldown": self.config().retrieval.access_cooldown_ms,
            }),
        )
        .await
    }

    /// 维护：把有效强度低于阈值的关系冻结（不参与检索），低于删除阈值的删除；
    /// 强度回升（重新被证据支撑）时 `write_graph` 会自动解冻。返回 (冻结数, 删除数)。
    pub async fn maintain(&self) -> Result<(usize, usize), MemoryError> {
        let now = now_ms();
        let rows: Vec<RelationRow> = self
            .query_rows(
                "SELECT meta::id(id) AS id, subject, predicate, object, paragraphs, strength,
                    anchor_ms, pinned FROM relation WHERE pinned = false",
                json!({}),
            )
            .await?;
        let half_life = self.config().half_life_hours;
        let (mut freeze, mut prune) = (Vec::new(), Vec::new());
        for row in rows {
            let s = effective_strength(&row, now, half_life);
            if s < self.config().prune_threshold {
                prune.push(row.id);
            } else if s < self.config().inactive_threshold {
                freeze.push(row.id);
            }
        }
        self.execute_vars(
            "UPDATE relation SET inactive = true WHERE meta::id(id) IN $freeze;
             DELETE relation WHERE meta::id(id) IN $prune;",
            json!({ "freeze": freeze, "prune": prune }),
        )
        .await?;
        Ok((freeze.len(), prune.len()))
    }

    /// 钉住 / 取消钉住一条关系：钉住后不衰减、不会被清理。
    pub async fn pin_relation(&self, id: &str, pinned: bool) -> Result<bool, MemoryError> {
        let now = now_ms();
        let updated: Vec<String> = self
            .query_rows(
                "UPDATE type::record('relation', $id) SET pinned = $pinned, strength = 1.0,
                    anchor_ms = $now, inactive = false RETURN VALUE meta::id(id)",
                json!({ "id": id, "pinned": pinned, "now": now }),
            )
            .await?;
        Ok(!updated.is_empty())
    }

    /// 某个实体的所有关系（含衰减后的有效强度），按强度降序。
    pub async fn relations_of(&self, entity: &str) -> Result<Vec<RelationView>, MemoryError> {
        let id = crate::memory::text::content_hash(&crate::memory::text::entity_key(entity));
        let rows: Vec<RelationRow> = self
            .query_rows(
                "LET $e = type::record('entity', $id);
                 SELECT meta::id(id) AS id, subject, predicate, object, paragraphs, strength,
                    anchor_ms, pinned FROM relation WHERE in = $e OR out = $e",
                json!({ "id": id }),
            )
            .await?;
        let now = now_ms();
        let half_life = self.config().half_life_hours;
        let mut views: Vec<RelationView> = rows
            .into_iter()
            .map(|r| RelationView {
                strength: effective_strength(&r, now, half_life),
                id: r.id,
                subject: r.subject,
                predicate: r.predicate,
                object: r.object,
                paragraphs: r.paragraphs,
            })
            .collect();
        views.sort_by(|a, b| b.strength.total_cmp(&a.strength));
        Ok(views)
    }
}

/// BM25 分数按本批最高分归一化到 0–1（BM25 绝对值不可跨查询比较）。
fn normalize_by_max(rows: Vec<Scored>) -> HashMap<String, f64> {
    let max = rows.iter().map(|r| r.score).fold(0.0, f64::max);
    rows.into_iter()
        .map(|r| {
            // 语料过小时 BM25 可能全为 0：命中即给满分
            let s = if max > 0.0 { r.score / max } else { 1.0 };
            (r.id, s)
        })
        .collect()
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
