//! 任务注册表：持久化所有会话的任务及其活动事件，并扇出全局任务事件流。
//!
//! 写路径：agent 的 [`EventObserver`] 同步回调 → 内存聚合（运行中任务、text 段合并）
//! → 无界队列 → 单个后台写库任务（保证顺序、不阻塞 agent）→ 全局 broadcast。
//! 读路径直接查库，不唤醒任何会话循环。

use crate::core::error::ApiError;
use agent::{AgentEvent, EventObserver, UsageView};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::sync::Mutex;
use surrealdb::Surreal;
use surrealdb::engine::any::Any;
use tokio::sync::{broadcast, mpsc};

const SCHEMA: &str = "
DEFINE TABLE IF NOT EXISTS task SCHEMALESS;
DEFINE INDEX IF NOT EXISTS task_owner ON task FIELDS owner, started_at_ms;
DEFINE INDEX IF NOT EXISTS task_session ON task FIELDS session_id;
DEFINE TABLE IF NOT EXISTS task_event SCHEMALESS;
DEFINE INDEX IF NOT EXISTS task_event_task ON task_event FIELDS task_key, seq;
";

/// 单个活动事件 `output` / `arguments` 落库前的最大字符数。
const MAX_EVENT_TEXT: usize = 16 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Done,
    Error,
    Cancelled,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TaskUsage {
    fn add(&mut self, u: &UsageView) {
        self.prompt_tokens += u64::from(u.prompt_tokens);
        self.completion_tokens += u64::from(u.completion_tokens);
        self.total_tokens += u64::from(u.total_tokens);
    }
}

/// 对外的任务视图（`GET /api/tasks`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TaskView {
    pub task_id: u64,
    pub session_id: String,
    pub worker: String,
    pub task: String,
    pub status: TaskStatus,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub usage: TaskUsage,
}

/// 持久化的活动事件（`GET /api/tasks/{sid}/{tid}` 的 `events`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TaskEvent {
    pub ts_ms: i64,
    #[serde(flatten)]
    pub body: Json,
}

/// 全局任务事件流里的一条：原事件 + `session_id`。
#[derive(Debug, Clone)]
pub struct GlobalEvent {
    pub name: &'static str,
    pub data: Json,
}

/// 写库指令（后台单任务顺序执行）。
enum Write {
    Upsert(TaskView, String),
    Event { key: String, seq: u64, event: TaskEvent },
}

/// 运行中任务的内存态。
struct Live {
    view: TaskView,
    owner: String,
    seq: u64,
    /// 尚未落库的 text 段：(agent, 文本, 段开始时间)。
    pending_text: Option<(String, String, i64)>,
}

#[derive(Clone)]
pub struct TaskRegistry {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    db: Surreal<Any>,
    writes: mpsc::UnboundedSender<Write>,
    live: Mutex<HashMap<String, Live>>,
    /// session_id → owner；会话打开时注册。
    owners: Mutex<HashMap<String, String>>,
    global: broadcast::Sender<(String, GlobalEvent)>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn task_key(session_id: &str, task_id: u64) -> String {
    format!("{session_id}_{task_id}")
}

/// 从 agent 标签取任务号：`coder#3` / `coder#3/sub1` → 3；主控等其他标签 → None。
fn task_of(agent: &str) -> Option<u64> {
    let head = agent.split('/').next()?;
    head.rsplit_once('#')?.1.parse().ok()
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_EVENT_TEXT) {
        Some((cut, _)) => format!("{}…[truncated]", &text[..cut]),
        None => text.to_owned(),
    }
}

impl TaskRegistry {
    /// 在给定连接上建表，并把上次进程退出时仍在 running 的任务标为 error
    /// （进程重启后它们不可能再完成）。
    pub async fn new(conn: &Surreal<Any>, namespace: &str, database: &str) -> Result<Self, surrealdb::Error> {
        let db = conn.clone();
        db.use_ns(namespace).use_db(database).await?;
        db.query(SCHEMA).await?.check()?;
        db.query("UPDATE task SET status = 'error', error = 'interrupted by server restart', finished_at_ms = $now WHERE status = 'running'")
            .bind(json!({ "now": now_ms() }))
            .await?
            .check()?;

        let (writes, mut rx) = mpsc::unbounded_channel::<Write>();
        let writer = db.clone();
        tokio::spawn(async move {
            while let Some(write) = rx.recv().await {
                let result = match write {
                    Write::Upsert(view, owner) => {
                        let key = task_key(&view.session_id, view.task_id);
                        let mut data = serde_json::to_value(&view).unwrap_or_default();
                        data["owner"] = json!(owner);
                        writer
                            .query("UPSERT type::record('task', $key) CONTENT $data")
                            .bind(json!({ "key": key, "data": data }))
                            .await
                            .and_then(|r| r.check())
                            .map(|_| ())
                    }
                    Write::Event { key, seq, event } => {
                        let mut data = serde_json::to_value(&event).unwrap_or_default();
                        data["task_key"] = json!(key);
                        data["seq"] = json!(seq);
                        writer
                            .query("CREATE task_event CONTENT $data")
                            .bind(json!({ "data": data }))
                            .await
                            .and_then(|r| r.check())
                            .map(|_| ())
                    }
                };
                if let Err(e) = result {
                    tracing::warn!("task registry write failed: {e}");
                }
            }
        });

        let (global, _) = broadcast::channel(2048);
        Ok(Self {
            inner: std::sync::Arc::new(Inner {
                db,
                writes,
                live: Mutex::new(HashMap::new()),
                owners: Mutex::new(HashMap::new()),
                global,
            }),
        })
    }

    /// 会话打开时登记归属。
    pub fn register_session(&self, session_id: &str, owner: &str) {
        self.inner.owners.lock().expect("owners poisoned").insert(session_id.to_owned(), owner.to_owned());
    }

    /// 订阅全局任务事件：`(owner, event)`，调用方按 owner 过滤。
    pub fn subscribe(&self) -> broadcast::Receiver<(String, GlobalEvent)> {
        self.inner.global.subscribe()
    }

    /// 会话内已用过的最大任务号（恢复会话时让 id 继续递增）。
    pub async fn max_task_id(&self, session_id: &str) -> Result<u64, ApiError> {
        let ids: Vec<u64> = self
            .query(
                "SELECT VALUE task_id FROM task WHERE session_id = $sid ORDER BY task_id DESC LIMIT 1",
                json!({ "sid": session_id }),
            )
            .await?;
        Ok(ids.first().copied().unwrap_or(0))
    }

    async fn query<T: serde::de::DeserializeOwned>(&self, sql: &str, vars: Json) -> Result<T, ApiError> {
        let mut response = self.inner.db.query(sql).bind(vars).await?.check()?;
        let last = response.num_statements().saturating_sub(1);
        let value: surrealdb::types::Value = response.take(last)?;
        Ok(serde_json::from_value(value.into_json_value())?)
    }

    /// 列出某用户的任务，按开始时间倒序，游标分页。
    pub async fn list(
        &self,
        owner: &str,
        statuses: &[TaskStatus],
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<TaskView>, Option<String>), ApiError> {
        let (before_ms, before_key) = match cursor {
            None => (i64::MAX, String::new()),
            Some(c) => decode_cursor(c).ok_or_else(|| ApiError::bad_request("invalid cursor"))?,
        };
        let mut rows: Vec<Json> = self
            .query(
                "SELECT *, meta::id(id) AS key FROM task
                 WHERE owner = $owner
                   AND (array::len($statuses) = 0 OR status IN $statuses)
                   AND (started_at_ms < $before_ms OR (started_at_ms = $before_ms AND meta::id(id) < $before_key))
                 ORDER BY started_at_ms DESC, key DESC LIMIT $limit",
                json!({
                    "owner": owner,
                    "statuses": statuses,
                    "before_ms": before_ms,
                    "before_key": before_key,
                    "limit": limit + 1,
                }),
            )
            .await?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|r| {
                encode_cursor(r["started_at_ms"].as_i64().unwrap_or(0), r["key"].as_str().unwrap_or(""))
            })
        } else {
            None
        };
        let mut tasks: Vec<TaskView> = rows
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()?;
        // 运行中任务的用量以内存为准（落库按事件节流）
        let live = self.inner.live.lock().expect("live poisoned");
        for t in &mut tasks {
            if let Some(l) = live.get(&task_key(&t.session_id, t.task_id)) {
                t.usage = l.view.usage;
            }
        }
        Ok((tasks, next))
    }

    /// 任务详情 + 活动事件（含尚未落库的 text 段）。不属于该用户时视同不存在。
    pub async fn detail(&self, owner: &str, session_id: &str, task_id: u64) -> Result<(TaskView, Vec<TaskEvent>), ApiError> {
        let key = task_key(session_id, task_id);
        let rows: Vec<TaskView> = self
            .query(
                "SELECT * FROM type::record('task', $key) WHERE owner = $owner",
                json!({ "key": key, "owner": owner }),
            )
            .await?;
        let mut view = rows.into_iter().next().ok_or_else(|| ApiError::not_found("task not found"))?;
        let mut events: Vec<TaskEvent> = self
            .query(
                "SELECT * OMIT id, task_key, seq FROM task_event WHERE task_key = $key ORDER BY seq",
                json!({ "key": key }),
            )
            .await?;
        if let Some(live) = self.inner.live.lock().expect("live poisoned").get(&key) {
            view.usage = live.view.usage;
            if let Some((agent, text, ts)) = &live.pending_text {
                events.push(TaskEvent { ts_ms: *ts, body: json!({ "kind": "text", "agent": agent, "text": text }) });
            }
        }
        Ok((view, events))
    }

    /// 任务当前状态（用于取消前判断 404 / 409）。
    pub async fn status(&self, owner: &str, session_id: &str, task_id: u64) -> Result<TaskStatus, ApiError> {
        let rows: Vec<TaskStatus> = self
            .query(
                "SELECT VALUE status FROM type::record('task', $key) WHERE owner = $owner",
                json!({ "key": task_key(session_id, task_id), "owner": owner }),
            )
            .await?;
        rows.into_iter().next().ok_or_else(|| ApiError::not_found("task not found"))
    }

    fn write(&self, w: Write) {
        let _ = self.inner.writes.send(w);
    }

    fn publish(&self, owner: &str, session_id: &str, name: &'static str, event: &AgentEvent) {
        let mut data = serde_json::to_value(event).unwrap_or_default();
        if let Some(obj) = data.as_object_mut() {
            obj.remove("type");
            obj.insert("session_id".into(), json!(session_id));
        }
        let _ = self.inner.global.send((owner.to_owned(), GlobalEvent { name, data }));
    }

    /// 把某任务待落库的 text 段写出。
    fn flush_text(&self, key: &str, live: &mut Live) {
        if let Some((agent, text, ts)) = live.pending_text.take() {
            live.seq += 1;
            self.write(Write::Event {
                key: key.to_owned(),
                seq: live.seq,
                event: TaskEvent { ts_ms: ts, body: json!({ "kind": "text", "agent": agent, "text": clip(&text) }) },
            });
        }
    }

    fn record(&self, key: &str, live: &mut Live, body: Json) {
        self.flush_text(key, live);
        live.seq += 1;
        self.write(Write::Event {
            key: key.to_owned(),
            seq: live.seq,
            event: TaskEvent { ts_ms: now_ms(), body },
        });
    }
}

impl EventObserver for TaskRegistry {
    fn observe(&self, session_id: &str, event: &AgentEvent) {
        let Some(owner) = self.inner.owners.lock().expect("owners poisoned").get(session_id).cloned() else {
            return;
        };
        let mut live = self.inner.live.lock().expect("live poisoned");
        match event {
            AgentEvent::TaskStarted { task_id, worker, task } => {
                let view = TaskView {
                    task_id: *task_id,
                    session_id: session_id.to_owned(),
                    worker: worker.clone(),
                    task: task.clone(),
                    status: TaskStatus::Running,
                    started_at_ms: now_ms(),
                    finished_at_ms: None,
                    result: None,
                    error: None,
                    usage: TaskUsage::default(),
                };
                self.write(Write::Upsert(view.clone(), owner.clone()));
                live.insert(task_key(session_id, *task_id), Live { view, owner: owner.clone(), seq: 0, pending_text: None });
                drop(live);
                self.publish(&owner, session_id, "task_started", event);
            }
            AgentEvent::TaskFinished { task_id, result, error, .. } => {
                let key = task_key(session_id, *task_id);
                if let Some(mut l) = live.remove(&key) {
                    self.flush_text(&key, &mut l);
                    l.view.status = match error.as_deref() {
                        None => TaskStatus::Done,
                        Some("cancelled") => TaskStatus::Cancelled,
                        Some(_) => TaskStatus::Error,
                    };
                    l.view.finished_at_ms = Some(now_ms());
                    l.view.result = result.clone();
                    l.view.error = error.clone();
                    self.write(Write::Upsert(l.view, l.owner));
                }
                drop(live);
                self.publish(&owner, session_id, "task_finished", event);
            }
            AgentEvent::TextDelta { agent, text } => {
                let Some(l) = task_of(agent).and_then(|t| live.get_mut(&task_key(session_id, t))) else {
                    return;
                };
                match &mut l.pending_text {
                    Some((a, buf, _)) if a == agent => buf.push_str(text),
                    _ => {
                        let key = task_key(session_id, l.view.task_id);
                        self.flush_text(&key, l);
                        l.pending_text = Some((agent.clone(), text.clone(), now_ms()));
                    }
                }
                drop(live);
                self.publish(&owner, session_id, "text_delta", event);
            }
            AgentEvent::ToolCall { agent, id, name, arguments } => {
                let Some(t) = task_of(agent) else { return };
                let key = task_key(session_id, t);
                let Some(l) = live.get_mut(&key) else { return };
                self.record(&key, l, json!({ "kind": "tool_call", "agent": agent, "id": id, "name": name, "arguments": clip(arguments) }));
                drop(live);
                self.publish(&owner, session_id, "tool_call", event);
            }
            AgentEvent::ToolResult { agent, id, name, output } => {
                let Some(t) = task_of(agent) else { return };
                let key = task_key(session_id, t);
                let Some(l) = live.get_mut(&key) else { return };
                self.record(&key, l, json!({ "kind": "tool_result", "agent": agent, "id": id, "name": name, "output": clip(output) }));
                drop(live);
                self.publish(&owner, session_id, "tool_result", event);
            }
            AgentEvent::Usage { agent, usage } => {
                let Some(t) = task_of(agent) else { return };
                let key = task_key(session_id, t);
                let Some(l) = live.get_mut(&key) else { return };
                l.view.usage.add(usage);
                self.record(&key, l, json!({ "kind": "usage", "agent": agent, "usage": usage }));
                // 运行中也落一次累计用量，进程意外退出时不至于全丢
                self.write(Write::Upsert(l.view.clone(), l.owner.clone()));
                drop(live);
                self.publish(&owner, session_id, "usage", event);
            }
            // 主控回复、推理碎片、错误不属于任务活动
            AgentEvent::ReasoningDelta { .. } | AgentEvent::Reply { .. } | AgentEvent::Error { .. } => {}
        }
    }
}

fn encode_cursor(started_at_ms: i64, key: &str) -> String {
    // 简单可读的不透明游标：十六进制编码 "ms:key"
    format!("{started_at_ms}:{key}").bytes().map(|b| format!("{b:02x}")).collect()
}

fn decode_cursor(cursor: &str) -> Option<(i64, String)> {
    if !cursor.len().is_multiple_of(2) || cursor.len() > 512 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..cursor.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cursor[i..i + 2], 16).ok())
        .collect();
    let text = String::from_utf8(bytes?).ok()?;
    let (ms, key) = text.split_once(':')?;
    Some((ms.parse().ok()?, key.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_labels() {
        assert_eq!(task_of("coder#3"), Some(3));
        assert_eq!(task_of("coder#3/sub1"), Some(3));
        assert_eq!(task_of("controller"), None);
    }

    #[test]
    fn cursor_roundtrip_and_rejects_garbage() {
        let c = encode_cursor(1791474802000, "abc_5");
        assert_eq!(decode_cursor(&c), Some((1791474802000, "abc_5".to_owned())));
        assert_eq!(decode_cursor("zz"), None);
        assert_eq!(decode_cursor("abc"), None);
    }
}
