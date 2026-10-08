//! 记忆存储：写入、三路检索、生命周期。

use crate::config::MemoryConfig;
use crate::embed::Embedder;
use crate::extract::{sanitize, Extractor};
use crate::retrieve::now_ms;
use crate::text::{content_hash, entity_key, normalize, relation_key, search_terms};
use crate::types::*;
use crate::MemoryError;
use serde::de::DeserializeOwned;
use serde_json::{json, Value as Json};
use std::sync::Arc;
use surrealdb::engine::any::{self, Any};
use surrealdb::Surreal;
use tracing::warn;

/// 持久化记忆器。`Clone` 廉价（共享连接）。
#[derive(Clone)]
pub struct MemoryStore {
    db: Surreal<Any>,
    config: Arc<MemoryConfig>,
    embedder: Option<Arc<dyn Embedder>>,
    extractor: Option<Arc<dyn Extractor>>,
}

impl MemoryStore {
    /// 连接（或创建）存储并初始化 schema。
    ///
    /// `embedder` 为 `None` 时只有全文 + 图通道；`extractor` 为 `None` 时不抽取三元组。
    pub async fn open(
        config: MemoryConfig,
        embedder: Option<Arc<dyn Embedder>>,
        extractor: Option<Arc<dyn Extractor>>,
    ) -> Result<Self, MemoryError> {
        let db = any::connect(config.endpoint.as_str()).await?;
        Self::with_connection(&db, config, embedder, extractor).await
    }

    /// 复用已有连接（如与后端共用同一个 SurrealKV 文件，避免重复打开抢文件锁）。
    /// `config.endpoint` 被忽略，namespace / database 仍取自 `config`。
    pub async fn with_connection(
        db: &Surreal<Any>,
        config: MemoryConfig,
        embedder: Option<Arc<dyn Embedder>>,
        extractor: Option<Arc<dyn Extractor>>,
    ) -> Result<Self, MemoryError> {
        if let Some(e) = &embedder
            && e.dimension() != config.embedding_dimension
        {
            return Err(MemoryError::Invalid(format!(
                "embedder dimension {} != configured {}",
                e.dimension(),
                config.embedding_dimension
            )));
        }
        // clone 得到独立 session，切 ns/db 不影响原连接
        let db = db.clone();
        db.use_ns(config.namespace.as_str()).use_db(config.database.as_str()).await?;
        let store = Self {
            db,
            config: Arc::new(config),
            embedder,
            extractor,
        };
        store.init_schema().await?;
        Ok(store)
    }

    /// 切到另一个记忆空间（同 namespace 下的另一个 database，如每个用户一个）。
    pub async fn scoped(&self, database: &str) -> Result<Self, MemoryError> {
        let valid = !database.is_empty()
            && database.len() <= 64
            && database.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !valid {
            return Err(MemoryError::Invalid(format!("bad memory space name {database:?}")));
        }
        let db = self.db.clone();
        db.use_ns(self.config.namespace.as_str()).use_db(database).await?;
        let store = Self { db, ..self.clone() };
        store.init_schema().await?;
        Ok(store)
    }

    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    async fn init_schema(&self) -> Result<(), MemoryError> {
        self.db
            .query(schema(self.config.embedding_dimension))
            .await?
            .check()?;
        Ok(())
    }

    /// 执行语句，把**最后一条**语句的结果解析成 `T`（前面可以有 `LET`）。
    pub(crate) async fn query_rows<T: DeserializeOwned>(
        &self,
        sql: &str,
        vars: Json,
    ) -> Result<T, MemoryError> {
        let mut response = self.db.query(sql).bind(vars).await?.check()?;
        let last = response.num_statements().saturating_sub(1);
        let value: surrealdb::types::Value = response.take(last)?;
        Ok(serde_json::from_value(value.into_json_value())?)
    }

    /// 执行若干语句，只检查错误。
    pub(crate) async fn execute_vars(&self, sql: &str, vars: Json) -> Result<(), MemoryError> {
        self.db.query(sql).bind(vars).await?.check()?;
        Ok(())
    }

    async fn query<T: DeserializeOwned>(&self, sql: &str, vars: Json) -> Result<T, MemoryError> {
        self.query_rows(sql, vars).await
    }

    async fn execute(&self, sql: &str, vars: Json) -> Result<(), MemoryError> {
        self.execute_vars(sql, vars).await
    }

    async fn embed_one(&self, text: &str) -> Result<Option<Vec<f32>>, MemoryError> {
        match &self.embedder {
            Some(e) => Ok(e.embed(&[text.to_owned()]).await?.pop()),
            None => Ok(None),
        }
    }

    /// 检索用的查询向量。
    pub(crate) async fn embed_query(&self, text: &str) -> Result<Option<Vec<f32>>, MemoryError> {
        self.embed_one(text).await
    }

    // ---- 读取 / 删除 ----

    /// 按 id 取一条记忆。
    pub async fn get(&self, id: &str) -> Result<Option<Memory>, MemoryError> {
        let rows: Vec<ParagraphRow> = self
            .query(
                "SELECT meta::id(id) AS id, content, source, tags, created_at_ms
                 FROM type::record('paragraph', $id)",
                json!({ "id": id }),
            )
            .await?;
        Ok(rows.into_iter().next().map(Memory::from))
    }

    /// 最近写入的记忆，新的在前。
    pub async fn recent(&self, limit: usize) -> Result<Vec<Memory>, MemoryError> {
        let rows: Vec<ParagraphRow> = self
            .query(
                "SELECT meta::id(id) AS id, content, source, tags, created_at_ms FROM paragraph
                 ORDER BY created_at_ms DESC LIMIT $limit",
                json!({ "limit": limit }),
            )
            .await?;
        Ok(rows.into_iter().map(Memory::from).collect())
    }

    /// 删除段落，并从关系的支撑列表里摘掉它；不再有任何支撑段落的关系一并删除
    /// （钉住的除外）。返回是否删除了段落。
    pub async fn delete(&self, id: &str) -> Result<bool, MemoryError> {
        let deleted: Vec<String> = self
            .query(
                "DELETE type::record('paragraph', $id) RETURN BEFORE",
                json!({ "id": id }),
            )
            .await
            .map(|rows: Vec<Json>| rows.iter().map(|r| r.to_string()).collect())?;
        self.detach_paragraphs(&[id.to_owned()]).await?;
        Ok(!deleted.is_empty())
    }

    /// 按来源批量删除，返回删除的段落数。
    pub async fn delete_by_source(&self, source: &str) -> Result<usize, MemoryError> {
        let ids: Vec<String> = self
            .query(
                "LET $ids = SELECT VALUE meta::id(id) FROM paragraph WHERE source = $source;
                 DELETE paragraph WHERE source = $source;
                 RETURN $ids;",
                json!({ "source": source }),
            )
            .await?;
        self.detach_paragraphs(&ids).await?;
        Ok(ids.len())
    }

    async fn detach_paragraphs(&self, ids: &[String]) -> Result<(), MemoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.execute(
            "UPDATE relation SET paragraphs = array::complement(paragraphs, $ids)
                WHERE paragraphs ANYINSIDE $ids;
             DELETE relation WHERE array::len(paragraphs) = 0 AND pinned = false;",
            json!({ "ids": ids }),
        )
        .await
    }

    /// 计数概况。
    pub async fn stats(&self) -> Result<MemoryStats, MemoryError> {
        let rows: Vec<MemoryStats> = self
            .query(
                "RETURN [{
                    paragraphs: count(SELECT id FROM paragraph),
                    pending_extraction: count(SELECT id FROM paragraph WHERE extracted = false),
                    entities: count(SELECT id FROM entity),
                    relations: count(SELECT id FROM relation),
                    inactive_relations: count(SELECT id FROM relation WHERE inactive = true),
                }]",
                json!({}),
            )
            .await?;
        rows.into_iter()
            .next()
            .ok_or_else(|| MemoryError::Invalid("empty stats".to_owned()))
    }

    // ---- 写入 ----

    /// 写入一条记忆。内容按规范化 hash 去重，`external_id` 幂等；重复写入只合并标签。
    ///
    /// 向量化在写入时同步完成（失败则整条写入失败）；三元组抽取只是排队，
    /// 由 [`process_pending`](Self::process_pending) 异步处理。
    pub async fn ingest(&self, request: IngestRequest) -> Result<IngestResult, MemoryError> {
        let content = normalize(&request.content);
        if content.is_empty() {
            return Err(MemoryError::Invalid("empty content".to_owned()));
        }
        let id = content_hash(&content);

        if let Some(external_id) = &request.external_id {
            let existing: Vec<String> = self
                .query(
                    "SELECT VALUE meta::id(id) FROM paragraph WHERE external_id = $external_id LIMIT 1",
                    json!({ "external_id": external_id }),
                )
                .await?;
            if let Some(existing) = existing.into_iter().next() {
                return Ok(IngestResult { id: existing, created: false });
            }
        }

        let exists: Vec<String> = self
            .query(
                "SELECT VALUE meta::id(id) FROM type::record('paragraph', $id)",
                json!({ "id": id }),
            )
            .await?;
        if !exists.is_empty() {
            self.execute(
                "UPDATE type::record('paragraph', $id) SET tags = array::union(tags ?? [], $tags)",
                json!({ "id": id, "tags": request.tags }),
            )
            .await?;
            return Ok(IngestResult { id, created: false });
        }

        let embedding = self.embed_one(&content).await?;
        let extract = request.extract && self.extractor.is_some();
        self.execute(
            "CREATE type::record('paragraph', $id) CONTENT $data",
            json!({
                "id": id,
                "data": {
                    "content": content,
                    "terms": search_terms(&content),
                    "embedding": embedding,
                    "source": request.source,
                    "external_id": request.external_id,
                    "tags": request.tags,
                    "created_at_ms": now_ms(),
                    // false = 待抽取；true = 已抽取或不需要
                    "extracted": !extract,
                }
            }),
        )
        .await?;
        Ok(IngestResult { id, created: true })
    }

    /// 处理至多 `limit` 条待抽取段落，返回处理条数。
    ///
    /// 单条抽取失败只记日志并标记为已处理（避免反复重试同一坏输入阻塞队列）。
    pub async fn process_pending(&self, limit: usize) -> Result<usize, MemoryError> {
        let Some(extractor) = &self.extractor else {
            return Ok(0);
        };
        let pending: Vec<ParagraphRow> = self
            .query(
                "SELECT meta::id(id) AS id, content FROM paragraph WHERE extracted = false LIMIT $limit",
                json!({ "limit": limit }),
            )
            .await?;
        for paragraph in &pending {
            match extractor.extract(&paragraph.content).await {
                Ok(extraction) => {
                    let extraction = sanitize(extraction, self.config.min_confidence);
                    self.write_graph(&paragraph.id, &extraction).await?;
                }
                Err(e) => warn!(paragraph = %paragraph.id, "extraction failed: {e}"),
            }
            self.execute(
                "UPDATE type::record('paragraph', $id) SET extracted = true",
                json!({ "id": paragraph.id }),
            )
            .await?;
        }
        Ok(pending.len())
    }

    /// 把一段的抽取结果写进图：实体 upsert，关系存在则并入支撑段落并重置强度。
    async fn write_graph(
        &self,
        paragraph_id: &str,
        extraction: &crate::extract::Extraction,
    ) -> Result<(), MemoryError> {
        for entity in &extraction.entities {
            self.execute(
                "UPSERT type::record('entity', $id) SET
                    name = name ?? $name,
                    kind = (IF kind = NONE OR kind = '' { $kind } ELSE { kind }),
                    mentions = (mentions ?? 0) + 1",
                json!({ "id": content_hash(&entity_key(&entity.name)), "name": entity.name, "kind": entity.kind }),
            )
            .await?;
        }

        let texts: Vec<String> = extraction
            .relations
            .iter()
            .map(|r| format!("{} {} {}", r.subject, r.predicate.replace('_', " "), r.object))
            .collect();
        let embeddings = match &self.embedder {
            Some(e) if !texts.is_empty() => e.embed(&texts).await?.into_iter().map(Some).collect(),
            _ => vec![None; texts.len()],
        };

        let now = now_ms();
        for ((relation, text), embedding) in extraction.relations.iter().zip(&texts).zip(embeddings) {
            self.execute(
                "LET $rid = type::record('relation', $id);
                 LET $s = type::record('entity', $subject_id);
                 LET $o = type::record('entity', $object_id);
                 IF record::exists($rid) {
                     UPDATE $rid SET
                         paragraphs = array::union(paragraphs, [$paragraph]),
                         confidence = math::max([confidence, $confidence]),
                         strength = 1.0, anchor_ms = $now, inactive = false
                 } ELSE {
                     RELATE $s->$rid->$o CONTENT {
                         subject: $subject, predicate: $predicate, object: $object,
                         terms: $terms, embedding: $embedding, confidence: $confidence,
                         paragraphs: [$paragraph], strength: 1.0, anchor_ms: $now,
                         last_access_ms: 0, pinned: false, inactive: false
                     }
                 }",
                json!({
                    "id": relation_key(&relation.subject, &relation.predicate, &relation.object),
                    "subject_id": content_hash(&entity_key(&relation.subject)),
                    "object_id": content_hash(&entity_key(&relation.object)),
                    "subject": relation.subject,
                    "predicate": relation.predicate,
                    "object": relation.object,
                    "terms": search_terms(text),
                    "embedding": embedding,
                    "confidence": relation.confidence,
                    "paragraph": paragraph_id,
                    "now": now,
                }),
            )
            .await?;
        }
        Ok(())
    }
}
