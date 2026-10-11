//! 心智端口的服务端实现：记忆、人、代谢、审计、认知（包装 agent::Runner）、网页表达。

use crate::core::budget::Budget;
use crate::core::db::Db;
use crate::mind::store::MindDb;
use agent::{EventSender, Runner};
use async_trait::async_trait;
use mind::*;
use model::{Message, Model};
use plugin::Registry;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use symbol::{IngestRequest, MemoryStore, Visibility};
use tokio::sync::broadcast;

fn port<E: std::fmt::Display>(e: E) -> PortError {
    PortError(e.to_string())
}

// ---- 记忆 ----

pub struct MemoryAdapter(pub MemoryStore);

#[async_trait]
impl MemoryPort for MemoryAdapter {
    async fn recall(&self, query: &str, limit: usize) -> PortResult<Vec<Recollection>> {
        let hits = self.0.search(query, limit).await.map_err(port)?;
        Ok(hits
            .into_iter()
            .map(|h| Recollection {
                id: h.id,
                content: h.content,
                about: h.about,
                visibility: serde_json::to_value(h.visibility).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default(),
                score: h.score,
            })
            .collect())
    }

    async fn remember(&self, content: &str, about: &[String], visibility: &str, source: &str) -> PortResult<String> {
        let visibility: Visibility = serde_json::from_value(json!(visibility)).unwrap_or_default();
        let r = self
            .0
            .ingest(IngestRequest::new(content).source(source).about(about.to_vec()).visibility(visibility))
            .await
            .map_err(port)?;
        Ok(r.id)
    }

    async fn untidiness(&self) -> PortResult<u64> {
        let s = self.0.stats().await.map_err(port)?;
        Ok(s.pending_extraction + s.inactive_relations)
    }
}

// ---- 人 ----

pub struct PeopleAdapter(pub Db);

#[async_trait]
impl People for PeopleAdapter {
    async fn get(&self, id: &str) -> PortResult<Option<Person>> {
        Ok(self.all().await?.into_iter().find(|p| p.id == id))
    }

    async fn all(&self) -> PortResult<Vec<Person>> {
        let users = self.0.list_users().await.map_err(|e| PortError(e.message))?;
        Ok(users.into_iter().map(|u| person(u.id, u.username)).collect())
    }
}

// ---- 代谢 ----

pub struct MetabolismAdapter(pub Budget);

impl Metabolism for MetabolismAdapter {
    fn fatigue(&self) -> f64 {
        self.0.status().fatigue
    }

    fn may_act_autonomously(&self) -> bool {
        self.0.may_act_autonomously()
    }
}

// ---- 审计 ----

pub struct AuditAdapter(pub Arc<MindDb>);

#[async_trait]
impl Auditor for AuditAdapter {
    async fn disclosure(&self, at_ms: i64, to: &str, memory_ids: &[String], text: &str) {
        self.0
            .record_audit(json!({ "at_ms": at_ms, "kind": "disclosure", "to": to, "memory_ids": memory_ids, "text": text }))
            .await;
    }
}

// ---- 认知 ----

/// 包装 `agent::Runner`：每个回合用当前工具集构造执行器。用量按回合性质记到
/// `user`（回应）或 `mind`（自主）名下，供代谢统计。
pub struct RunnerCognition {
    pub model: Model,
    pub max_turns: usize,
    pub max_tool_output_chars: usize,
    pub events: EventSender,
}

/// 两个固定的"会话 id"：心智回合的用量据此归入不同来源。
pub const RESPONSIVE_SESSION: &str = "mind:responsive";
pub const AUTONOMOUS_SESSION: &str = "mind:autonomous";

#[async_trait]
impl Cognition for RunnerCognition {
    async fn think(&self, kind: TurnKind, history: &mut Vec<Message>, tools: &Registry) -> PortResult<String> {
        let events = match kind {
            TurnKind::Responsive => self.events.for_session(RESPONSIVE_SESSION),
            TurnKind::Autonomous => self.events.for_session(AUTONOMOUS_SESSION),
        };
        let runner = Runner::new("mind", self.model.clone(), tools.clone(), self.max_turns, self.max_tool_output_chars, events);
        runner.run(history).await.map_err(port)
    }
}

// ---- 网页表达 ----

/// 一个人看到的对话事件。
#[derive(serde::Serialize, Debug, Clone)]
pub struct Delivery {
    pub to: String,
    pub moment: Moment,
}

/// 网页表达：在线的人通过 SSE 实时收到；不在线的人下次打开时从意识流读取（`/api/mind/conversation`）。
#[derive(Clone, Default)]
pub struct WebExpression {
    inner: Arc<WebInner>,
}

#[derive(Default)]
struct WebInner {
    /// person id → 打开的连接数
    present: Mutex<HashMap<String, usize>>,
    tx: Mutex<Option<broadcast::Sender<Delivery>>>,
}

/// 在线连接的守卫：丢弃时计数减一。
pub struct Presence {
    web: WebExpression,
    person: String,
}

impl Drop for Presence {
    fn drop(&mut self) {
        let mut p = self.web.inner.present.lock().expect("presence poisoned");
        if let Some(n) = p.get_mut(&self.person) {
            *n -= 1;
            if *n == 0 {
                p.remove(&self.person);
            }
        }
    }
}

impl WebExpression {
    pub fn new() -> Self {
        let web = Self::default();
        *web.inner.tx.lock().expect("tx poisoned") = Some(broadcast::channel(1024).0);
        web
    }

    /// 某人打开了对话页。
    pub fn connect(&self, person_id: &str) -> (Presence, broadcast::Receiver<Delivery>) {
        *self.inner.present.lock().expect("presence poisoned").entry(person_id.to_owned()).or_default() += 1;
        let rx = self.sender().subscribe();
        (Presence { web: self.clone(), person: person_id.to_owned() }, rx)
    }

    fn sender(&self) -> broadcast::Sender<Delivery> {
        self.inner.tx.lock().expect("tx poisoned").clone().expect("initialized")
    }

    /// 把一个时刻推给相关的人（他自己说的话回显也走这里）。
    pub fn push(&self, to: &str, moment: &Moment) {
        let _ = self.sender().send(Delivery { to: to.to_owned(), moment: moment.clone() });
    }

    pub fn present_ids(&self) -> Vec<String> {
        self.inner.present.lock().expect("presence poisoned").keys().cloned().collect()
    }
}

#[async_trait]
impl Expression for WebExpression {
    async fn deliver(&self, moment: &Moment, expressed: &Expressed) -> PortResult<()> {
        if let Expressed::Said { to, .. } = expressed {
            self.push(to, moment);
        }
        Ok(())
    }

    fn is_present(&self, person_id: &str) -> bool {
        self.inner.present.lock().expect("presence poisoned").contains_key(person_id)
    }
}
