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
use tokio::sync::{broadcast, mpsc};

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
    /// 一批感知连续失败多少次后放弃（记一笔、不再重试）。
    pub max_retries: u32,
    /// 意识流里单条时刻渲染的字符上限。
    pub max_moment_chars: usize,
    /// 注入上下文的意识流总字符上限。
    pub max_stream_chars: usize,
    /// 她的身体与环境（宿主填写，注入 system）。
    #[serde(skip)]
    pub body: String,
}

/// 回想检索线索的字符上限。
const MAX_CUE_CHARS: usize = 2000;

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
            max_retries: 5,
            max_moment_chars: 4000,
            max_stream_chars: 60_000,
            body: String::new(),
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

/// 唤醒心智循环的信号（感知已写入意识流）。
enum Wake {
    /// 有新感知；`drive` 为点火的驱动名（如果是驱动感知）。
    Percept { drive: Option<String> },
    /// 失败退避后重试积压的感知。
    Retry,
}

/// 心智句柄。`Clone` 廉价；所有句柄丢弃后心智停止。
///
/// 感知在调用方这边立即写入意识流（不等正在进行的回合），然后唤醒心智循环；
/// 自我模型放在共享锁里，读取也不经过循环。所以她在长回合里时，
/// 别人说话、看状态、心跳都不会被卡住。
#[derive(Clone)]
pub struct Mind {
    wake: mpsc::Sender<Wake>,
    shared: Arc<Shared>,
    events: broadcast::Sender<MindEvent>,
    outcomes: broadcast::Sender<(Option<String>, Outcome)>,
    thinking: Arc<std::sync::atomic::AtomicBool>,
}

/// 句柄与循环共享的状态。
struct Shared {
    stream: Arc<dyn StreamStore>,
    clock: Arc<dyn Clock>,
    self_model: std::sync::RwLock<SelfModel>,
    /// 感知写入串行化：保证 seq 顺序与"认识的人"更新一致。
    record: tokio::sync::Mutex<()>,
}

impl Mind {
    /// 启动心智：加载自我、写入苏醒感知、开始循环。
    pub async fn start(config: MindConfig, ports: Ports) -> PortResult<Self> {
        let (wake, rx) = mpsc::channel(config.inbox_capacity);
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

        let shared = Arc::new(Shared {
            stream: Arc::clone(&ports.stream),
            clock: Arc::clone(&ports.clock),
            self_model: std::sync::RwLock::new(self_model),
            record: tokio::sync::Mutex::new(()),
        });
        let mind = Self {
            wake: wake.clone(),
            shared: Arc::clone(&shared),
            events: events.clone(),
            outcomes: outcomes.clone(),
            thinking: Arc::clone(&thinking),
        };
        let core = Core { config, ports, shared, events, outcomes, thinking, failures: 0, retry: wake.downgrade() };
        tokio::spawn(core.run(rx));
        mind.perceive(Percept::Awake { slept_ms: slept, first }).await?;
        Ok(mind)
    }

    /// 投递一条感知：立即写入意识流并返回该时刻（不等她当前的回合结束）。
    pub async fn perceive(&self, percept: Percept) -> PortResult<Moment> {
        let triggers = percept.triggers();
        let drive = match &percept {
            Percept::Drive { drive, .. } => Some(drive.clone()),
            _ => None,
        };
        let moment = {
            let _guard = self.shared.record.lock().await;
            let now = self.shared.clock.now_ms();
            if let Percept::Heard { from, .. } = &percept {
                let mut model = self.shared.self_model.write().expect("self model poisoned");
                let a = model.people.entry(from.id.clone()).or_insert_with(|| Acquaintance {
                    name: from.name.clone(),
                    ..Default::default()
                });
                a.last_seen_ms = now;
                a.exchanges += 1;
            }
            self.shared.stream.append(now, MomentBody::In { percept }).await?
        };
        let _ = self.events.send(MindEvent::Moment { moment: moment.clone() });
        if triggers {
            // 队列满说明循环已有大量待唤醒信号：丢掉这一个无妨，感知已在流里
            let _ = self.wake.try_send(Wake::Percept { drive });
        }
        Ok(moment)
    }

    /// 投递一条感知，不等待写入结果（在后台写入）。
    pub fn perceive_nowait(&self, percept: Percept) -> bool {
        let mind = self.clone();
        tokio::spawn(async move {
            if let Err(e) = mind.perceive(percept).await {
                tracing::warn!("perceive failed: {e}");
            }
        });
        true
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

    /// 当前自我模型的快照（不等待回合）。
    pub async fn self_model(&self) -> PortResult<SelfModel> {
        if self.wake.is_closed() {
            return Err(PortError("mind stopped".into()));
        }
        Ok(self.shared.self_model.read().expect("self model poisoned").clone())
    }
}

struct Core {
    config: MindConfig,
    ports: Ports,
    shared: Arc<Shared>,
    events: broadcast::Sender<MindEvent>,
    outcomes: broadcast::Sender<(Option<String>, Outcome)>,
    thinking: Arc<std::sync::atomic::AtomicBool>,
    failures: u32,
    retry: mpsc::WeakSender<Wake>,
}

impl Core {
    async fn run(mut self, mut rx: mpsc::Receiver<Wake>) {
        // 等一个唤醒；之后把积压的唤醒全部收下（一次回合处理意识流里所有未回应的）
        while let Some(first) = rx.recv().await {
            let mut drive = None;
            let mut wakes = vec![first];
            while let Ok(more) = rx.try_recv() {
                wakes.push(more);
            }
            for w in wakes {
                if let Wake::Percept { drive: Some(d) } = w {
                    drive = Some(d);
                }
            }
            self.turn(drive).await;
        }
    }

    fn model(&self) -> SelfModel {
        self.shared.self_model.read().expect("self model poisoned").clone()
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
        let log = Arc::new(Mutex::new(TurnLog::default()));
        let result = self.think(kind, &fresh, Arc::clone(&log)).await;
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
            Err(e) => {
                let said = !log.lock().expect("turn log poisoned").said_to.is_empty();
                if said || self.failures + 1 >= self.config.max_retries {
                    // 她已经开口了（重试会重复说话），或这批感知总是失败：不再重试，记下就过去
                    let _ = self.ports.stream.mark_attended(last_seq).await;
                    self.failures = 0;
                    let note = if said { "说到一半，思绪断了。" } else { "刚才怎么也想不清楚，先放下了。" };
                    let _ = self.append_out(Expressed::Thought { text: note.into() }).await;
                    let _ = self.events.send(MindEvent::TurnFailed { error: e.0, backoff_secs: None });
                } else {
                    self.fail(e.0).await;
                }
            }
        }
    }

    async fn append_out(&self, expressed: Expressed) -> PortResult<Moment> {
        let m = self.ports.stream.append(self.ports.clock.now_ms(), MomentBody::Out { expressed }).await?;
        let _ = self.events.send(MindEvent::Moment { moment: m.clone() });
        Ok(m)
    }

    async fn think(&mut self, kind: TurnKind, fresh: &[Moment], log: Arc<Mutex<TurnLog>>) -> PortResult<Outcome> {
        let now = self.ports.clock.now_ms();
        let people = self.ports.people.all().await?;
        let names: Names = people.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
        {
            let mut model = self.shared.self_model.write().expect("self model poisoned");
            for p in &people {
                model.people.entry(p.id.clone()).or_insert_with(|| Acquaintance { name: p.name.clone(), ..Default::default() });
            }
        }
        let self_model = self.model();
        let present: Vec<String> =
            people.iter().filter(|p| self.ports.expression.is_present(&p.id)).map(|p| p.name.clone()).collect();

        // 工作记忆：摘要之后、最近的若干条
        let through = self.ports.stream.summarized_through().await?;
        let mut working = self.ports.stream.latest(self.config.working_memory).await?;
        working.retain(|m| m.seq > through);
        let summaries = self.ports.stream.summaries(self.config.summaries).await?;

        // 回想：以这次新到的感知为线索（截断：检索只需要要点）
        let cue: String = fresh.iter().filter_map(Moment::percept).filter_map(Percept::cue).collect::<Vec<_>>().join("\n");
        let cue: String = cue.chars().take(MAX_CUE_CHARS).collect();
        let recollections = match (&self.ports.memory, cue.is_empty()) {
            (Some(memory), false) => memory.recall(&cue, self.config.recall).await.unwrap_or_default(),
            _ => Vec::new(),
        };

        let system = system_prompt(&ContextParts {
            origin: &self.config.origin,
            self_model: &self_model,
            now_ms: now,
            tz_offset_min: self.config.tz_offset_min,
            summaries: &summaries,
            recollections: &recollections,
            present: &present,
            names: &names,
            body: &self.config.body,
        });
        // 工作记忆 + 还没回应的感知（可能比工作记忆更早，例如她睡了很久）；
        // 单条与总量都有字符上限，防止一条超长消息撑爆上下文
        let first_fresh = fresh.first().map_or(u64::MAX, |m| m.seq);
        let mut shown: Vec<&Moment> = working.iter().filter(|m| m.seq < first_fresh).collect();
        shown.extend(fresh.iter().rev().take(self.config.working_memory).rev());
        let mut lines: Vec<String> = Vec::new();
        let mut budget = self.config.max_stream_chars;
        for m in shown.iter().rev() {
            let mut line = render_moment(m, &names, self.config.tz_offset_min);
            if line.chars().count() > self.config.max_moment_chars {
                let cut: String = line.chars().take(self.config.max_moment_chars).collect();
                line = format!("{cut}…[太长，后面省略了]");
            }
            let len = line.chars().count();
            if len > budget && !lines.is_empty() {
                break;
            }
            budget = budget.saturating_sub(len);
            if m.seq == first_fresh {
                lines.push("—— 以下是你还没回应的 ——".into());
                lines.reverse();
                lines.push(line);
                lines.reverse();
                continue;
            }
            lines.push(line);
        }
        lines.reverse();
        let mut stream_text = String::new();
        for l in &lines {
            if l.starts_with("—— 以下") {
                stream_text.push('\n');
            }
            stream_text.push_str(l);
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
            log,
            self_model: Mutex::new(self_model.clone()),
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
        let thought = thought.trim();

        // 兜底：有人在等她回应，她却一句话都没对人说、只写了"心里话"——多半是模型忘了用 say。
        // 把这段话对最近对她说话的人说出来，免得对方什么都收不到。
        let speaker = fresh.iter().rev().find_map(|m| match m.percept() {
            Some(Percept::Heard { from, .. }) => Some(from.id.clone()),
            _ => None,
        });
        let said_nothing = turn.log.lock().expect("turn log poisoned").said_to.is_empty();
        if said_nothing
            && !thought.is_empty()
            && let Some(to) = speaker
        {
            tracing::info!("turn ended without say; delivering final text to {to}");
            let expressed = Expressed::Said { to: to.clone(), text: thought.to_owned() };
            let m = self.append_out(expressed.clone()).await?;
            if let Err(e) = self.ports.expression.deliver(&m, &expressed).await {
                tracing::warn!("fallback delivery failed: {e}");
            }
            turn.log.lock().expect("turn log poisoned").said_to.push(to);
        } else if !thought.is_empty() {
            // 收尾：心里话进流
            self.append_out(Expressed::Thought { text: thought.to_owned() }).await?;
        }

        // 自我模型：回合内 self_reflect 的改动合并回共享模型。
        // people 的 last_seen / exchanges 在回合期间可能被新感知更新，按较大值合并
        let log = std::mem::take(&mut *turn.log.lock().expect("turn log poisoned"));
        let mut new_model = turn.self_model.lock().expect("self model poisoned").clone();
        let changed = {
            let mut shared = self.shared.self_model.write().expect("self model poisoned");
            for (id, a) in new_model.people.iter_mut() {
                if let Some(cur) = shared.people.get(id) {
                    a.last_seen_ms = a.last_seen_ms.max(cur.last_seen_ms);
                    a.exchanges = a.exchanges.max(cur.exchanges);
                }
            }
            for (id, cur) in &shared.people {
                new_model.people.entry(id.clone()).or_insert_with(|| cur.clone());
            }
            let changed = new_model != *shared;
            *shared = new_model.clone();
            changed
        };
        if changed {
            self.ports.selves.save(&new_model, None).await?;
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
                let _ = tx.send(Wake::Retry).await;
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
