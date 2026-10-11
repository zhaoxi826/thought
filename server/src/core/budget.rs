//! 代谢：token 用量计量、防失控硬上限、急停。
//!
//! 作为 [`EventObserver`] 接收所有 `Usage` 事件，按 UTC 日 × 来源累计，
//! 内存聚合后由后台任务定期落库（不阻塞 agent）。
//!
//! - 来源（`origin`）：`user`（人发起的对话及其派生任务）/ `mind`（她自主发起，阶段 1 心跳点火）。
//!   硬上限只约束 `mind`：超过后她不再自主醒来，但人找她说话照常。
//! - 急停（`paused`）：停止一切自主活动；持久化，重启后保持。

use crate::core::error::ApiError;
use agent::{AgentEvent, EventObserver};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use surrealdb::Surreal;
use surrealdb::engine::any::Any;

const SCHEMA: &str = "
DEFINE TABLE IF NOT EXISTS usage_day SCHEMALESS;
DEFINE TABLE IF NOT EXISTS mind_flag SCHEMALESS;
";

/// 预算配置（`thought.toml` 的 `[budget]`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct BudgetConfig {
    /// 自主活动单日 token 软额度：接近时她更"困"（阶段 1 的代谢驱动据此调制阈值）。
    pub daily_soft_tokens: u64,

    /// 自主活动单日 token 硬上限：超过后不再自主醒来。防 bug 失控，默认很高。
    pub daily_hard_tokens: u64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_soft_tokens: 2_000_000,
            daily_hard_tokens: 10_000_000,
        }
    }
}

/// 某天某来源的累计。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub calls: u64,
}

/// 对外的用量视图。
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BudgetStatus {
    pub day: String,
    pub by_origin: BTreeMap<String, Tally>,
    pub mind_tokens_today: u64,
    pub soft_limit: u64,
    pub hard_limit: u64,
    /// 0–1：今日自主用量 / 软额度（可超过 1）。
    pub fatigue: f64,
    pub over_hard_limit: bool,
    pub paused: bool,
}

#[derive(Clone)]
pub struct Budget {
    inner: Arc<Inner>,
}

struct Inner {
    db: Surreal<Any>,
    config: Mutex<BudgetConfig>,
    /// (day, origin) → 累计；只保留今天与少量近期，持久化后可从库读历史。
    tallies: Mutex<BTreeMap<(String, String), Tally>>,
    dirty: AtomicBool,
    paused: AtomicBool,
    /// session_id → origin；未登记的会话视为 `user`。
    origins: Mutex<BTreeMap<String, String>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// UTC 日期 `YYYY-MM-DD`。
pub fn utc_day(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

impl Budget {
    /// 建表、读回今天的累计与急停状态，启动后台落库任务（每 10 秒，有变化才写）。
    pub async fn new(conn: &Surreal<Any>, namespace: &str, database: &str, config: BudgetConfig) -> Result<Self, surrealdb::Error> {
        let db = conn.clone();
        db.use_ns(namespace).use_db(database).await?;
        db.query(SCHEMA).await?.check()?;

        let today = utc_day(now_ms());
        let mut response = db
            .query("SELECT origin, prompt_tokens, completion_tokens, total_tokens, calls FROM usage_day WHERE day = $day;
                    SELECT VALUE paused FROM mind_flag:state;")
            .bind(json!({ "day": today }))
            .await?
            .check()?;
        let rows: surrealdb::types::Value = response.take(0)?;
        let paused: surrealdb::types::Value = response.take(1)?;
        let mut tallies = BTreeMap::new();
        if let serde_json::Value::Array(rows) = rows.into_json_value() {
            for row in rows {
                if let (Some(origin), Ok(t)) = (row["origin"].as_str().map(str::to_owned), serde_json::from_value::<Tally>(row.clone())) {
                    tallies.insert((today.clone(), origin), t);
                }
            }
        }
        let paused = matches!(paused.into_json_value(), serde_json::Value::Array(v) if v.first() == Some(&json!(true)));

        let budget = Self {
            inner: Arc::new(Inner {
                db,
                config: Mutex::new(config),
                tallies: Mutex::new(tallies),
                dirty: AtomicBool::new(false),
                paused: AtomicBool::new(paused),
                origins: Mutex::new(BTreeMap::new()),
            }),
        };
        let flusher = budget.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                tick.tick().await;
                if let Err(e) = flusher.flush().await {
                    tracing::warn!("budget flush failed: {e}");
                }
            }
        });
        Ok(budget)
    }

    /// 登记会话的来源（`user` / `mind`）。
    pub fn set_origin(&self, session_id: &str, origin: &str) {
        self.inner.origins.lock().expect("origins poisoned").insert(session_id.to_owned(), origin.to_owned());
    }

    pub fn set_config(&self, config: BudgetConfig) {
        *self.inner.config.lock().expect("budget config poisoned") = config;
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    /// 急停 / 恢复，立即持久化。
    pub async fn set_paused(&self, paused: bool) -> Result<(), ApiError> {
        self.inner.paused.store(paused, Ordering::Relaxed);
        self.inner
            .db
            .query("UPSERT mind_flag:state SET paused = $paused, changed_at_ms = $now")
            .bind(json!({ "paused": paused, "now": now_ms() }))
            .await?
            .check()?;
        Ok(())
    }

    /// 她现在能否自主醒来：未急停且未超硬上限。
    pub fn may_act_autonomously(&self) -> bool {
        let status = self.status();
        !status.paused && !status.over_hard_limit
    }

    pub fn status(&self) -> BudgetStatus {
        let day = utc_day(now_ms());
        let config = self.inner.config.lock().expect("budget config poisoned").clone();
        let tallies = self.inner.tallies.lock().expect("tallies poisoned");
        let by_origin: BTreeMap<String, Tally> = tallies
            .iter()
            .filter(|((d, _), _)| *d == day)
            .map(|((_, o), t)| (o.clone(), *t))
            .collect();
        let mind = by_origin.get("mind").map_or(0, |t| t.total_tokens);
        BudgetStatus {
            fatigue: if config.daily_soft_tokens == 0 { 0.0 } else { mind as f64 / config.daily_soft_tokens as f64 },
            over_hard_limit: mind >= config.daily_hard_tokens,
            mind_tokens_today: mind,
            soft_limit: config.daily_soft_tokens,
            hard_limit: config.daily_hard_tokens,
            paused: self.is_paused(),
            by_origin,
            day,
        }
    }

    /// 写回有变化的累计；顺带丢掉内存里早于今天的条目。
    pub async fn flush(&self) -> Result<(), surrealdb::Error> {
        if !self.inner.dirty.swap(false, Ordering::Relaxed) {
            return Ok(());
        }
        let today = utc_day(now_ms());
        let snapshot: Vec<((String, String), Tally)> = {
            let mut tallies = self.inner.tallies.lock().expect("tallies poisoned");
            let snapshot = tallies.iter().map(|(k, v)| (k.clone(), *v)).collect();
            tallies.retain(|(day, _), _| *day >= today);
            snapshot
        };
        for ((day, origin), t) in snapshot {
            self.inner
                .db
                .query("UPSERT type::record('usage_day', $id) CONTENT $data")
                .bind(json!({
                    "id": format!("{day}_{origin}"),
                    "data": { "day": day, "origin": origin, "prompt_tokens": t.prompt_tokens,
                              "completion_tokens": t.completion_tokens, "total_tokens": t.total_tokens, "calls": t.calls },
                }))
                .await?
                .check()?;
        }
        Ok(())
    }

    /// 最近若干天的按日用量（含今天的内存值）。
    pub async fn history(&self, days: usize) -> Result<Vec<serde_json::Value>, ApiError> {
        let _ = self.flush().await;
        let mut response = self
            .inner
            .db
            .query("SELECT day, origin, prompt_tokens, completion_tokens, total_tokens, calls FROM usage_day ORDER BY day DESC LIMIT $n")
            .bind(json!({ "n": days * 2 }))
            .await?
            .check()?;
        let rows: surrealdb::types::Value = response.take(0)?;
        Ok(serde_json::from_value(rows.into_json_value())?)
    }
}

impl EventObserver for Budget {
    fn observe(&self, session_id: &str, event: &AgentEvent) {
        let AgentEvent::Usage { usage, .. } = event else { return };
        let origin = self
            .inner
            .origins
            .lock()
            .expect("origins poisoned")
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| "user".to_owned());
        let day = utc_day(now_ms());
        let mut tallies = self.inner.tallies.lock().expect("tallies poisoned");
        let t = tallies.entry((day, origin)).or_default();
        t.prompt_tokens += u64::from(usage.prompt_tokens);
        t.completion_tokens += u64::from(usage.completion_tokens);
        t.total_tokens += u64::from(usage.total_tokens);
        t.calls += 1;
        self.inner.dirty.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::utc_day;

    #[test]
    fn utc_day_matches_known_dates() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1_791_676_800_000), "2026-10-11");
        assert_eq!(utc_day(1_791_676_800_000 - 1), "2026-10-10");
    }
}
