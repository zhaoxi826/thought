//! 心智循环：感知进入意识流 → 串行回合 → 表达。唯一、连续（docs/mind-design.md §1、§5.4）。

use crate::core::context::{render_moment, system_prompt, ContextParts, Names};
use crate::core::ports::*;
use crate::core::stream::{Expressed, Moment, MomentBody, Percept, Person, Summary};
use crate::heart::Outcome;
use crate::self_model::{Acquaintance, Origin, SelfModel};
use crate::tools::{self, Deps, TurnLog, TurnState};
use model::Message;
use plugin::Registry;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};

/// 心智配置。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct MindConfig {
    pub origin: Origin,
    /// 工作记忆：注入原文的最近时刻数上限。
    pub working_memory: usize,
    /// 工作记忆超过此数时触发压缩，把最早的一半压成摘要。
    pub compact_at: usize,
    /// 注入的摘要段数。
    pub summaries: usize,
    /// 每个回合回想的长期记忆条数。
    pub recall: usize,
    /// 时区偏移（分钟），用于渲染时间；默认 UTC+8。
    pub tz_offset_min: i32,
    /// 感知队列容量（防洪）。
    pub inbox_capacity: usize,
    /// 连续失败多少次后熔断退避。
    pub breaker_threshold: u32,
}

impl Default for MindConfig {
    fn default() -> Self {
        Self {
            origin: Origin::default(),
            working_memory: 60,
            compact_at: 80,
            summaries: 8,
            recall: 6,
            tz_offset_min: 8 * 60,
            inbox_capacity: 256,
            breaker_threshold: 3,
        }
    }
}

/// 心智对外广播的事件（前端"心"页、admin 透明视图用）。
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MindEvent {
    /// 意识流上新增了一个时刻。
    Moment { moment: Moment },
    /// 她开始想 / 想完了。
    Thinking { active: bool },
    /// 回合失败（已熔断时附退避秒数）。
    TurnFailed { error: String, backoff_secs: Option<u64> },
    /// 自我模型变了。
    SelfChanged,
    /// 她改了自己的核心（identity / values）：宿主应通知管理员。
    CoreChanged { fields: Vec<String> },
}

/// 外部端口集合。
pub struct Ports {
    pub stream: Arc<dyn StreamStore>,
    pub selves: Arc<dyn SelfStore>,
    pub memory: Option<Arc<dyn MemoryPort>>,
    pub expression: Arc<dyn Expression>,
    pub metabolism: Arc<dyn Metabolism>,
    pub auditor: Arc<dyn Auditor>,
    pub people: Arc<dyn People>,
    pub cognition: Arc<dyn Cognition>,
    pub clock: Arc<dyn Clock>,
    /// 她可用的其他工具（沙箱文件 / 命令、记忆、派单……）。
    pub tools: Registry,
    /// 用于压缩摘要的廉价认知（缺省用 `cognition`）。
    pub summarizer: Option<Arc<dyn Cognition>>,
}

enum Command {
    Perceive(Percept, Option<oneshot::Sender<PortResult<Moment>>>),
    /// 失败退避后重试积压的感知（不写入新感知）。
    Retry,
    /// 回合结束后由驱动层读取。
    Snapshot(oneshot::Sender<SelfModel>),
}

/// 心智句柄。`Clone` 廉价；所有句柄丢弃后心智停止。
#[derive(Clone)]
pub struct Mind {
    tx: mpsc::Sender<Command>,
    events: broadcast::Sender<MindEvent>,
    outcomes: broadcast::Sender<(Option<String>, Outcome)>,
    thinking: Arc<std::sync::atomic::AtomicBool>,
}

impl Mind {
    /// 启动心智：加载自我、写入苏醒感知、开始循环。
    pub async fn start(config: MindConfig, ports: Ports) -> PortResult<Self> {
        let (tx, rx) = mpsc::channel(config.inbox_capacity);
        let (events, _) = broadcast::channel(1024);
        let (outcomes, _) = broadcast::channel(64);
        let thinking = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let now = ports.clock.now_ms();

        let (self_model, first) = match ports.selves.load().await? {
            Some(m) => (m, false),
            None => {
                let m = SelfModel::seeded(&config.origin, now);
                ports.selves.save(&m, None).await?;
                (m, true)
            }
        };
        let last = ports.stream.latest(1).await?.pop();
        let slept = last.map_or(0, |m| now - m.at_ms);

        let mind = Self { tx: tx.clone(), events: events.clone(), outcomes: outcomes.clone(), thinking: Arc::clone(&thinking) };
        let core = Core { config, ports, self_model, events, outcomes, thinking, failures: 0, retry: tx.downgrade() };
        tokio::spawn(core.run(rx));
        mind.perceive(Percept::Awake { slept_ms: slept, first }).await?;
        Ok(mind)
    }

    /// 投递一条感知，返回写入意识流后的时刻。
    pub async fn perceive(&self, percept: Percept) -> PortResult<Moment> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Perceive(percept, Some(reply)))
            .await
            .map_err(|_| PortError("mind stopped".into()))?;
        rx.await.map_err(|_| PortError("mind stopped".into()))?
    }

    /// 投递一条感知，不等待（队列满时丢弃并返回 false）。
    pub fn perceive_nowait(&self, percept: Percept) -> bool {
        self.tx.try_send(Command::Perceive(percept, None)).is_ok()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MindEvent> {
        self.events.subscribe()
    }

    /// 回合结束时的结果：`(点火的驱动名, 结果)`，供驱动判断是否被满足。
    pub fn outcomes(&self) -> broadcast::Receiver<(Option<String>, Outcome)> {
        self.outcomes.subscribe()
    }

    pub fn is_thinking(&self) -> bool {
        self.thinking.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 当前自我模型的快照。
    pub async fn self_model(&self) -> PortResult<SelfModel> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Command::Snapshot(reply)).await.map_err(|_| PortError("mind stopped".into()))?;
        rx.await.map_err(|_| PortError("mind stopped".into()))
    }
}

struct Core {
    config: MindConfig,
    ports: Ports,
    self_model: SelfModel,
    events: broadcast::Sender<MindEvent>,
    outcomes: broadcast::Sender<(Option<String>, Outcome)>,
    thinking: Arc<std::sync::atomic::AtomicBool>,
    failures: u32,
    retry: mpsc::WeakSender<Command>,
}

impl Core {
    async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        loop {
            // 等第一条命令；之后把积压的全部收下（一次回合处理一批）
            let Some(first) = rx.recv().await else { return };
            let mut pending = false;
            let mut drive: Option<String> = None;
            let mut cmds = vec![first];
            while let Ok(more) = rx.try_recv() {
                cmds.push(more);
            }
            for cmd in cmds {
                match cmd {
                    Command::Snapshot(reply) => {
                        let _ = reply.send(self.self_model.clone());
                    }
                    Command::Retry => pending = true,
                    Command::Perceive(percept, reply) => {
                        if let Percept::Drive { drive: d, .. } = &percept {
                            drive = Some(d.clone());
                        }
                        pending |= percept.triggers();
                        let result = self.record(percept).await;
                        if let Some(reply) = reply {
                            let _ = reply.send(result);
                        }
                    }
                }
            }
            if pending {
                self.turn(drive).await;
            }
        }
    }

    /// 把感知写进意识流，并更新"认识的人"。
    async fn record(&mut self, percept: Percept) -> PortResult<Moment> {
        let now = self.ports.clock.now_ms();
        if let Percept::Heard { from, .. } = &percept {
            let a = self.self_model.people.entry(from.id.clone()).or_insert_with(|| Acquaintance {
                name: from.name.clone(),
                ..Default::default()
            });
            a.last_seen_ms = now;
            a.exchanges += 1;
        }
        let moment = self.ports.stream.append(now, MomentBody::In { percept }).await?;
        let _ = self.events.send(MindEvent::Moment { moment: moment.clone() });
        Ok(moment)
    }

    /// 一个回合：组装上下文 → 思考 → 收尾。
    async fn turn(&mut self, drive: Option<String>) {
        let attended = self.ports.stream.attended_through().await.unwrap_or(0);
        let fresh = match self.ports.stream.since(attended, 1000).await {
            Ok(f) => f,
            Err(e) => return self.fail(format!("read stream: {e}")).await,
        };
        let Some(last_seq) = fresh.last().map(|m| m.seq) else { return };
        let responsive = fresh.iter().filter_map(Moment::percept).any(Percept::responsive);
        let kind = if responsive { TurnKind::Responsive } else { TurnKind::Autonomous };
        if kind == TurnKind::Autonomous && !self.ports.metabolism.may_act_autonomously() {
            // 急停或超硬上限：自主念头只记下，不展开
            let _ = self.ports.stream.mark_attended(last_seq).await;
            return;
        }

        self.set_thinking(true);
        let result = self.think(kind, &fresh).await;
        self.set_thinking(false);
        match result {
            Ok(outcome) => {
                self.failures = 0;
                let _ = self.ports.stream.mark_attended(last_seq).await;
                let _ = self.outcomes.send((drive, outcome));
                if let Err(e) = self.compact().await {
                    tracing::warn!("compaction failed: {e}");
                }
            }
            Err(e) => self.fail(e.0).await,
        }
    }

    async fn think(&mut self, kind: TurnKind, fresh: &[Moment]) -> PortResult<Outcome> {
        let now = self.ports.clock.now_ms();
        let people = self.ports.people.all().await?;
        let names: Names = people.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
        for p in &people {
            self.self_model.people.entry(p.id.clone()).or_insert_with(|| Acquaintance { name: p.name.clone(), ..Default::default() });
        }
        let present: Vec<String> =
            people.iter().filter(|p| self.ports.expression.is_present(&p.id)).map(|p| p.name.clone()).collect();

        // 工作记忆：摘要之后、最近的若干条
        let through = self.ports.stream.summarized_through().await?;
        let mut working = self.ports.stream.latest(self.config.working_memory).await?;
        working.retain(|m| m.seq > through);
        let summaries = self.ports.stream.summaries(self.config.summaries).await?;

        // 回想：以这次新到的感知为线索
        let cue: String = fresh.iter().filter_map(Moment::percept).filter_map(Percept::cue).collect::<Vec<_>>().join("\n");
        let recollections = match (&self.ports.memory, cue.is_empty()) {
            (Some(memory), false) => memory.recall(&cue, self.config.recall).await.unwrap_or_default(),
            _ => Vec::new(),
        };

        let system = system_prompt(&ContextParts {
            origin: &self.config.origin,
            self_model: &self.self_model,
            now_ms: now,
            tz_offset_min: self.config.tz_offset_min,
            summaries: &summaries,
            recollections: &recollections,
            present: &present,
            names: &names,
        });
        let attended = fresh.first().map_or(u64::MAX, |m| m.seq);
        let mut stream_text = String::new();
        for m in &working {
            if m.seq == attended {
                stream_text.push_str("\n—— 以下是你还没回应的 ——\n");
            }
            stream_text.push_str(&render_moment(m, &names, self.config.tz_offset_min));
            stream_text.push('\n');
        }
        let mut history = vec![
            Message::system(system),
            Message::user(format!(
                "## 意识流\n{stream_text}\n现在轮到你了。需要对谁说话就用 say；想做什么就去做；\
                 最后写下的文字是你的心里话，不会发给任何人。"
            )),
        ];

        // 回合工具：心智工具 + 外部工具
        let turn = Arc::new(TurnState {
            log: Mutex::new(TurnLog::default()),
            self_model: Mutex::new(self.self_model.clone()),
            now_ms: {
                let clock = Arc::clone(&self.ports.clock);
                Box::new(move || clock.now_ms())
            },
        });
        let deps = Arc::new(Deps {
            stream: Arc::clone(&self.ports.stream),
            selves: Arc::clone(&self.ports.selves),
            expression: Arc::clone(&self.ports.expression),
            known: people.iter().map(|p| p.id.clone()).collect(),
        });
        let mut registry = self.ports.tools.clone();
        tools::register(&mut registry, deps, Arc::clone(&turn));

        let thought = self.ports.cognition.think(kind, &mut history, &registry).await?;

        // 收尾：心里话进流；自我模型落库；隐私审计
        let thought = thought.trim();
        if !thought.is_empty() {
            let m = self
                .ports
                .stream
                .append(self.ports.clock.now_ms(), MomentBody::Out { expressed: Expressed::Thought { text: thought.to_owned() } })
                .await?;
            let _ = self.events.send(MindEvent::Moment { moment: m });
        }
        let log = std::mem::take(&mut *turn.log.lock().expect("turn log poisoned"));
        let new_model = turn.self_model.lock().expect("self model poisoned").clone();
        let changed = new_model != self.self_model;
        self.self_model = new_model;
        if changed {
            self.ports.selves.save(&self.self_model, None).await?;
            if log.reflected {
                let _ = self.events.send(MindEvent::SelfChanged);
            }
            if !log.core_changed.is_empty() {
                let _ = self.events.send(MindEvent::CoreChanged { fields: log.core_changed.clone() });
            }
        }
        self.audit(&log, &recollections, &history).await;

        let tools_used = history
            .iter()
            .filter_map(|m| match m {
                Message::Assistant { tool_calls, .. } => Some(tool_calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()),
                _ => None,
            })
            .flatten()
            .collect();
        Ok(Outcome { said_to: log.said_to, tools_used, reflected: log.reflected })
    }

    /// 只记录：她对某人说话时，本回合想起过涉及别人的 private 记忆。
    async fn audit(&self, log: &TurnLog, recollections: &[Recollection], history: &[Message]) {
        if log.said_to.is_empty() {
            return;
        }
        let said: Vec<(String, String)> = history
            .iter()
            .filter_map(|m| match m {
                Message::Assistant { tool_calls, .. } => Some(tool_calls),
                _ => None,
            })
            .flatten()
            .filter(|c| c.name == "say")
            .filter_map(|c| {
                let v: serde_json::Value = serde_json::from_str(&c.arguments).ok()?;
                Some((v["to"].as_str()?.to_owned(), v["text"].as_str()?.to_owned()))
            })
            .collect();
        let now = self.ports.clock.now_ms();
        for (to, text) in said {
            let others: Vec<String> = recollections
                .iter()
                .filter(|r| r.visibility == "private" && !r.about.is_empty())
                .filter(|r| !r.about.iter().any(|a| a.trim_start_matches("user:") == to))
                .map(|r| r.id.clone())
                .collect();
            if !others.is_empty() {
                self.ports.auditor.disclosure(now, &to, &others, &text).await;
            }
        }
    }

    /// 工作记忆超限时，把最早的一半压成摘要。
    async fn compact(&mut self) -> PortResult<()> {
        let through = self.ports.stream.summarized_through().await?;
        let window = self.ports.stream.since(through, self.config.compact_at + 1).await?;
        if window.len() <= self.config.compact_at {
            return Ok(());
        }
        let take = window.len() / 2;
        let chunk = &window[..take];
        let names: Names = self.ports.people.all().await?.into_iter().map(|p| (p.id, p.name)).collect();
        let text: String = chunk.iter().map(|m| render_moment(m, &names, self.config.tz_offset_min) + "\n").collect();
        let mut history = vec![
            Message::system(format!(
                "你是{}。下面是你一段较早的经历。用第一人称写一段简短的摘要（不超过 150 字）：\
                 发生了什么、和谁、你的感受与决定。只写摘要本身。",
                self.config.origin.name
            )),
            Message::user(text),
        ];
        let cognition = self.ports.summarizer.as_ref().unwrap_or(&self.ports.cognition);
        let summary = cognition.think(TurnKind::Autonomous, &mut history, &Registry::new()).await?;
        let (first, last) = (&chunk[0], &chunk[take - 1]);
        self.ports
            .stream
            .save_summary(Summary {
                from_seq: first.seq,
                to_seq: last.seq,
                from_ms: first.at_ms,
                to_ms: last.at_ms,
                text: summary.trim().to_owned(),
            })
            .await?;
        if let Some(memory) = &self.ports.memory {
            let about: Vec<String> = chunk
                .iter()
                .filter_map(Moment::person)
                .map(|p| format!("user:{p}"))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let _ = memory.remember(summary.trim(), &about, "private", "mind:summary").await;
        }
        Ok(())
    }

    async fn fail(&mut self, error: String) {
        self.failures += 1;
        tracing::warn!(failures = self.failures, "mind turn failed: {error}");
        let backoff = (self.failures >= self.config.breaker_threshold)
            .then(|| 60u64.saturating_mul(1 << (self.failures - self.config.breaker_threshold).min(5)));
        let _ = self.events.send(MindEvent::TurnFailed { error, backoff_secs: backoff });
        // 失败的感知仍未处理：退避后自己再试一次（不阻塞循环，期间新感知照常进流）
        let delay = Duration::from_secs(backoff.unwrap_or(5));
        let retry = self.retry.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Some(tx) = retry.upgrade() {
                let _ = tx.send(Command::Retry).await;
            }
        });
    }

    fn set_thinking(&self, active: bool) {
        self.thinking.store(active, std::sync::atomic::Ordering::Relaxed);
        let _ = self.events.send(MindEvent::Thinking { active });
    }
}

/// 便于宿主构造 `Person`。
pub fn person(id: impl Into<String>, name: impl Into<String>) -> Person {
    Person { id: id.into(), name: name.into() }
}
