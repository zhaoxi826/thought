//! 意识流与自我的 SurrealDB 持久化。

use crate::core::error::ApiError;
use async_trait::async_trait;
use mind::core::stream::MomentBody;
use mind::{Moment, PortError, PortResult, Scheduled, SelfChange, SelfModel, SelfStore, StreamStore, Summary};
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};
use std::sync::Mutex;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

const SCHEMA: &str = "
DEFINE TABLE IF NOT EXISTS stream SCHEMALESS;
DEFINE INDEX IF NOT EXISTS stream_seq ON stream FIELDS seq UNIQUE;
DEFINE INDEX IF NOT EXISTS stream_person ON stream FIELDS person, seq;
DEFINE TABLE IF NOT EXISTS stream_summary SCHEMALESS;
DEFINE INDEX IF NOT EXISTS stream_summary_to ON stream_summary FIELDS to_seq;
DEFINE TABLE IF NOT EXISTS mind_state SCHEMALESS;
DEFINE TABLE IF NOT EXISTS self_change SCHEMALESS;
DEFINE INDEX IF NOT EXISTS self_change_at ON self_change FIELDS at_ms;
DEFINE TABLE IF NOT EXISTS journal SCHEMALESS;
DEFINE INDEX IF NOT EXISTS journal_at ON journal FIELDS at_ms;
DEFINE TABLE IF NOT EXISTS scheduled SCHEMALESS;
DEFINE INDEX IF NOT EXISTS scheduled_due ON scheduled FIELDS due_ms;
DEFINE TABLE IF NOT EXISTS audit SCHEMALESS;
DEFINE INDEX IF NOT EXISTS audit_at ON audit FIELDS at_ms;
";

fn port<E: std::fmt::Display>(e: E) -> PortError {
    PortError(e.to_string())
}

/// 心智的持久化：意识流、摘要、自我模型、变化历史、日记、待办、审计。
pub struct MindDb {
    db: Surreal<Any>,
    /// 下一个 seq；追加串行化，保证单调。
    next_seq: Mutex<u64>,
}

impl MindDb {
    pub async fn new(conn: &Surreal<Any>, namespace: &str, database: &str) -> Result<Self, surrealdb::Error> {
        let db = conn.clone();
        db.use_ns(namespace).use_db(database).await?;
        db.query(SCHEMA).await?.check()?;
        let mut r = db.query("SELECT VALUE seq FROM stream ORDER BY seq DESC LIMIT 1").await?.check()?;
        let v: surrealdb::types::Value = r.take(0)?;
        let last = v.into_json_value().as_array().and_then(|a| a.first()).and_then(Json::as_u64).unwrap_or(0);
        Ok(Self { db, next_seq: Mutex::new(last + 1) })
    }

    pub(crate) async fn query<T: DeserializeOwned>(&self, sql: &str, vars: Json) -> PortResult<T> {
        let mut response = self.db.query(sql).bind(vars).await.map_err(port)?.check().map_err(port)?;
        let last = response.num_statements().saturating_sub(1);
        let value: surrealdb::types::Value = response.take(last).map_err(port)?;
        serde_json::from_value(value.into_json_value()).map_err(port)
    }

    async fn exec(&self, sql: &str, vars: Json) -> PortResult<()> {
        self.db.query(sql).bind(vars).await.map_err(port)?.check().map_err(port)?;
        Ok(())
    }

    async fn state<T: DeserializeOwned + Default>(&self, key: &str) -> PortResult<T> {
        let rows: Vec<Json> = self
            .query("SELECT VALUE value FROM type::record('mind_state', $key)", json!({ "key": key }))
            .await?;
        Ok(rows.into_iter().next().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default())
    }

    async fn set_state<T: serde::Serialize>(&self, key: &str, value: &T) -> PortResult<()> {
        self.exec("UPSERT type::record('mind_state', $key) SET value = $value", json!({ "key": key, "value": value }))
            .await
    }

    /// 某人能看到的对话：他说的 + 对他说的，按 seq 升序，`seq > after`。
    pub async fn conversation(&self, person_id: &str, after: u64, limit: usize) -> Result<Vec<Moment>, ApiError> {
        self.query(
            "SELECT * OMIT id, person FROM stream WHERE person = $p AND seq > $after ORDER BY seq LIMIT $limit",
            json!({ "p": person_id, "after": after, "limit": limit }),
        )
        .await
        .map_err(ApiError::internal)
    }

    /// 完整意识流（admin 透明视图），`seq < before`，按 seq 降序。
    pub async fn stream_page(&self, before: u64, limit: usize) -> Result<Vec<Moment>, ApiError> {
        self.query(
            "SELECT * OMIT id, person FROM stream WHERE seq < $before ORDER BY seq DESC LIMIT $limit",
            json!({ "before": before, "limit": limit }),
        )
        .await
        .map_err(ApiError::internal)
    }

    pub async fn journal_page(&self, before_ms: i64, limit: usize) -> Result<Vec<Json>, ApiError> {
        self.query(
            "SELECT at_ms, text FROM journal WHERE at_ms < $before ORDER BY at_ms DESC LIMIT $limit",
            json!({ "before": before_ms, "limit": limit }),
        )
        .await
        .map_err(ApiError::internal)
    }

    pub async fn audit_page(&self, limit: usize) -> Result<Vec<Json>, ApiError> {
        self.query("SELECT * OMIT id FROM audit ORDER BY at_ms DESC LIMIT $limit", json!({ "limit": limit }))
            .await
            .map_err(ApiError::internal)
    }

    pub async fn record_audit(&self, entry: Json) {
        if let Err(e) = self.exec("CREATE audit CONTENT $data", json!({ "data": entry })).await {
            tracing::warn!("audit write failed: {e}");
        }
    }
}

#[async_trait]
impl StreamStore for MindDb {
    async fn append(&self, at_ms: i64, body: MomentBody) -> PortResult<Moment> {
        // seq 分配与写入在同一把锁内，保证单调且无空洞
        let seq = {
            let mut next = self.next_seq.lock().expect("seq lock poisoned");
            let seq = *next;
            *next += 1;
            seq
        };
        let moment = Moment { seq, at_ms, body };
        let mut data = serde_json::to_value(&moment).map_err(port)?;
        data["person"] = json!(moment.person());
        self.exec("CREATE type::record('stream', $seq) CONTENT $data", json!({ "seq": seq, "data": data })).await?;
        Ok(moment)
    }

    async fn since(&self, after: u64, limit: usize) -> PortResult<Vec<Moment>> {
        self.query(
            "SELECT * OMIT id, person FROM stream WHERE seq > $after ORDER BY seq LIMIT $limit",
            json!({ "after": after, "limit": limit }),
        )
        .await
    }

    async fn latest(&self, limit: usize) -> PortResult<Vec<Moment>> {
        let mut rows: Vec<Moment> = self
            .query("SELECT * OMIT id, person FROM stream ORDER BY seq DESC LIMIT $limit", json!({ "limit": limit }))
            .await?;
        rows.reverse();
        Ok(rows)
    }

    async fn with_person(&self, person_id: &str, before: u64, limit: usize) -> PortResult<Vec<Moment>> {
        self.query(
            "SELECT * OMIT id, person FROM stream WHERE person = $p AND seq < $before ORDER BY seq DESC LIMIT $limit",
            json!({ "p": person_id, "before": before, "limit": limit }),
        )
        .await
    }

    async fn save_summary(&self, summary: Summary) -> PortResult<()> {
        self.exec("CREATE stream_summary CONTENT $data", json!({ "data": summary })).await
    }

    async fn summaries(&self, limit: usize) -> PortResult<Vec<Summary>> {
        let mut rows: Vec<Summary> = self
            .query("SELECT * OMIT id FROM stream_summary ORDER BY to_seq DESC LIMIT $limit", json!({ "limit": limit }))
            .await?;
        rows.reverse();
        Ok(rows)
    }

    async fn summarized_through(&self) -> PortResult<u64> {
        let rows: Vec<u64> = self
            .query("SELECT VALUE to_seq FROM stream_summary ORDER BY to_seq DESC LIMIT 1", json!({}))
            .await?;
        Ok(rows.first().copied().unwrap_or(0))
    }

    async fn attended_through(&self) -> PortResult<u64> {
        self.state("attended").await
    }

    async fn mark_attended(&self, seq: u64) -> PortResult<()> {
        self.set_state("attended", &seq).await
    }
}

#[async_trait]
impl SelfStore for MindDb {
    async fn load(&self) -> PortResult<Option<SelfModel>> {
        self.state("self_model").await
    }

    async fn save(&self, model: &SelfModel, change: Option<SelfChange>) -> PortResult<()> {
        self.set_state("self_model", &Some(model)).await?;
        if let Some(change) = change {
            self.exec("CREATE self_change CONTENT $data", json!({ "data": change })).await?;
        }
        Ok(())
    }

    async fn history(&self, limit: usize) -> PortResult<Vec<SelfChange>> {
        self.query("SELECT * OMIT id FROM self_change ORDER BY at_ms DESC LIMIT $limit", json!({ "limit": limit }))
            .await
    }

    async fn write_journal(&self, at_ms: i64, text: &str) -> PortResult<()> {
        self.exec("CREATE journal CONTENT $data", json!({ "data": { "at_ms": at_ms, "text": text } })).await
    }

    async fn schedule(&self, item: Scheduled) -> PortResult<()> {
        self.exec("CREATE scheduled CONTENT $data", json!({ "data": item })).await
    }

    async fn take_due(&self, now_ms: i64) -> PortResult<Vec<Scheduled>> {
        let rows: Vec<Scheduled> = self
            .query(
                "LET $due = SELECT * OMIT id FROM scheduled WHERE due_ms <= $now ORDER BY due_ms;
                 DELETE scheduled WHERE due_ms <= $now;
                 RETURN $due;",
                json!({ "now": now_ms }),
            )
            .await?;
        Ok(rows)
    }
}
