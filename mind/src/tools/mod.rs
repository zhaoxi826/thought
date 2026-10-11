//! 心智自带的工具：说话、修改自我、写日记、安排待办。
//!
//! 工具只做"意图记录"：把动作推入回合的 [`TurnLog`]，由心智循环在回合结束后
//! 统一写意识流、投递、保存（保证顺序与原子性，也便于测试）。`say` 例外：
//! 它立刻写入意识流并投递，好让在线的人实时听到。

use crate::core::ports::{Expression, Scheduled, SelfChange, SelfStore, StreamStore};
use crate::core::stream::{Expressed, MomentBody};
use crate::self_model::SelfModel;
use model::{Function, Parameter, ParameterType};
use plugin::{Registry, Tool};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

/// 一个回合里发生的事，用于驱动满足判断与审计。
#[derive(Debug, Default)]
pub struct TurnLog {
    pub said_to: Vec<String>,
    pub reflected: bool,
    pub tools_used: Vec<String>,
    /// 本回合改过的核心字段（identity / values）。
    pub core_changed: Vec<String>,
}

/// 回合内共享的可变状态。
pub(crate) struct TurnState {
    pub log: Arc<Mutex<TurnLog>>,
    pub self_model: Mutex<SelfModel>,
    pub now_ms: Box<dyn Fn() -> i64 + Send + Sync>,
}

pub(crate) struct Deps {
    pub stream: Arc<dyn StreamStore>,
    pub selves: Arc<dyn SelfStore>,
    pub expression: Arc<dyn Expression>,
    /// 认识的人 id；`say` 只允许对他们说。
    pub known: Vec<String>,
}

macro_rules! parse_args {
    ($tool:literal, $arguments:expr) => {
        match serde_json::from_str($arguments) {
            Ok(args) => args,
            Err(e) => return format!("invalid arguments for {}: {e}", $tool),
        }
    };
}

fn s(name: &str, desc: &str) -> Parameter {
    Parameter::required(name, desc, ParameterType::String)
}

/// 注册心智工具。
pub(crate) fn register(registry: &mut Registry, deps: Arc<Deps>, turn: Arc<TurnState>) {
    registry
        .register(Arc::new(Say { deps: Arc::clone(&deps), turn: Arc::clone(&turn) }))
        .register(Arc::new(SelfReflect { deps: Arc::clone(&deps), turn: Arc::clone(&turn) }))
        .register(Arc::new(Journal { deps: Arc::clone(&deps), turn: Arc::clone(&turn) }))
        .register(Arc::new(Schedule { deps, turn }));
}

// ---- say ----

struct Say {
    deps: Arc<Deps>,
    turn: Arc<TurnState>,
}

#[derive(Deserialize)]
struct SayArgs {
    to: String,
    text: String,
}

#[async_trait::async_trait]
impl Tool for Say {
    fn name(&self) -> &str {
        "say"
    }

    fn definition(&self) -> Function {
        Function {
            name: "say".to_owned(),
            description: "Say something to a person. `to` is their id. If they are not online they \
                          will see it next time they come. This is the only way to talk to anyone."
                .to_owned(),
            parameters: vec![s("to", "Person id"), s("text", "What you say")],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: SayArgs = parse_args!("say", arguments);
        let text = args.text.trim();
        if text.is_empty() {
            return "nothing to say".to_owned();
        }
        if !self.deps.known.iter().any(|k| k == &args.to) {
            return format!("you don't know anyone with id {:?}", args.to);
        }
        let expressed = Expressed::Said { to: args.to.clone(), text: text.to_owned() };
        let now = (self.turn.now_ms)();
        let moment = match self.deps.stream.append(now, MomentBody::Out { expressed: expressed.clone() }).await {
            Ok(m) => m,
            Err(e) => return format!("could not say it: {e}"),
        };
        let delivered = self.deps.expression.deliver(&moment, &expressed).await;
        {
            let mut log = self.turn.log.lock().expect("turn log poisoned");
            log.said_to.push(args.to.clone());
        }
        {
            let mut model = self.turn.self_model.lock().expect("self model poisoned");
            if let Some(a) = model.people.get_mut(&args.to) {
                a.last_seen_ms = now;
                a.exchanges += 1;
            }
        }
        match (delivered, self.deps.expression.is_present(&args.to)) {
            (Err(e), _) => format!("said, but delivery failed: {e}"),
            (Ok(()), true) => "said (they are here)".to_owned(),
            (Ok(()), false) => "said (they are away and will see it later)".to_owned(),
        }
    }
}

// ---- self_reflect ----

struct SelfReflect {
    deps: Arc<Deps>,
    turn: Arc<TurnState>,
}

#[derive(Deserialize)]
struct ReflectArgs {
    /// identity / values / interests / mood / projects / people.<id>
    field: String,
    value: serde_json::Value,
    #[serde(default)]
    reason: String,
}

#[async_trait::async_trait]
impl Tool for SelfReflect {
    fn name(&self) -> &str {
        "self_reflect"
    }

    fn definition(&self) -> Function {
        Function {
            name: "self_reflect".to_owned(),
            description: "Change how you understand yourself. `field` is one of identity (string), \
                          values (array of strings), interests (array of {topic, why}), mood \
                          ({valence -1..1, arousal 0..1, note}), projects (array of {id, title, status \
                          active|paused|done, next}), or people.<id> ({name, impression, open_threads}). \
                          The value replaces the whole field. Give a reason: it is kept as your history."
                .to_owned(),
            parameters: vec![
                s("field", "Which part of yourself"),
                Parameter::required("value", "New value (JSON)", ParameterType::Object),
                s("reason", "Why you are changing it"),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: ReflectArgs = parse_args!("self_reflect", arguments);
        let now = (self.turn.now_ms)();
        // 身份护栏：核心（我是谁、我在意什么）要写理由，且一天只能改一次——
        // 防止被人反复劝说、或一时冲动，就变成另一个人
        if CORE_FIELDS.contains(&args.field.as_str()) {
            if args.reason.trim().chars().count() < 8 {
                return format!("changing your {} needs a real reason (why, in a sentence or two)", args.field);
            }
            let last = match self.deps.selves.history(200).await {
                Ok(h) => h.iter().filter(|c| c.field == args.field).map(|c| c.at_ms).max().unwrap_or(0),
                Err(e) => return format!("could not check history: {e}"),
            };
            if last > 0 && now - last < CORE_COOLDOWN_MS {
                let hours = (CORE_COOLDOWN_MS - (now - last)) / 3_600_000 + 1;
                return format!(
                    "you changed your {} less than a day ago; sit with it for about {hours} more hour(s) before changing it again",
                    args.field
                );
            }
        }
        let (before, after, model) = {
            let mut model = self.turn.self_model.lock().expect("self model poisoned");
            let mut json = match serde_json::to_value(&*model) {
                Ok(j) => j,
                Err(e) => return format!("internal error: {e}"),
            };
            let before;
            if let Some(id) = args.field.strip_prefix("people.") {
                // 合并，保留系统维护的 last_seen_ms / exchanges
                let people = &mut json["people"];
                before = people.get(id).cloned().unwrap_or(serde_json::Value::Null);
                let mut merged = before.clone();
                if !merged.is_object() {
                    merged = serde_json::json!({});
                }
                if let (Some(m), Some(v)) = (merged.as_object_mut(), args.value.as_object()) {
                    for (k, val) in v {
                        if k != "last_seen_ms" && k != "exchanges" {
                            m.insert(k.clone(), val.clone());
                        }
                    }
                } else {
                    return "people.<id> expects an object".to_owned();
                }
                people[id] = merged;
            } else {
                match args.field.as_str() {
                    "identity" | "values" | "interests" | "mood" | "projects" => {}
                    other => return format!("unknown field {other:?}"),
                }
                before = json[&args.field].clone();
                json[&args.field] = args.value.clone();
            }
            json["updated_ms"] = serde_json::json!(now);
            let new: SelfModel = match serde_json::from_value(json.clone()) {
                Ok(m) => m,
                Err(e) => return format!("that doesn't fit {}: {e}", args.field),
            };
            let after = if let Some(id) = args.field.strip_prefix("people.") {
                json["people"][id].clone()
            } else {
                json[&args.field].clone()
            };
            *model = new.clone();
            (before, after, new)
        };
        let change = SelfChange { at_ms: now, field: args.field.clone(), before, after, reason: args.reason };
        if let Err(e) = self.deps.selves.save(&model, Some(change)).await {
            return format!("could not save: {e}");
        }
        {
            let mut log = self.turn.log.lock().expect("turn log poisoned");
            log.reflected = true;
            if CORE_FIELDS.contains(&args.field.as_str()) {
                log.core_changed.push(args.field.clone());
            }
        }
        format!("updated {}", args.field)
    }
}

/// 构成"我是谁"的核心字段：修改受理由与冷却约束，并通知管理员。
pub const CORE_FIELDS: &[&str] = &["identity", "values"];

/// 核心字段两次修改的最短间隔。
pub const CORE_COOLDOWN_MS: i64 = 24 * 3_600_000;

// ---- journal ----

struct Journal {
    deps: Arc<Deps>,
    turn: Arc<TurnState>,
}

#[derive(Deserialize)]
struct JournalArgs {
    text: String,
}

#[async_trait::async_trait]
impl Tool for Journal {
    fn name(&self) -> &str {
        "journal"
    }

    fn definition(&self) -> Function {
        Function {
            name: "journal".to_owned(),
            description: "Write a short diary entry for yourself: what happened, what you thought, \
                          what you want to remember."
                .to_owned(),
            parameters: vec![s("text", "The entry")],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: JournalArgs = parse_args!("journal", arguments);
        match self.deps.selves.write_journal((self.turn.now_ms)(), args.text.trim()).await {
            Ok(()) => "written".to_owned(),
            Err(e) => format!("could not write: {e}"),
        }
    }
}

// ---- schedule ----

struct Schedule {
    deps: Arc<Deps>,
    turn: Arc<TurnState>,
}

#[derive(Deserialize)]
struct ScheduleArgs {
    text: String,
    #[serde(default)]
    in_minutes: u64,
}

/// 最长 30 天。
const MAX_SCHEDULE_MIN: u64 = 30 * 24 * 60;

#[async_trait::async_trait]
impl Tool for Schedule {
    fn name(&self) -> &str {
        "schedule"
    }

    fn definition(&self) -> Function {
        Function {
            name: "schedule".to_owned(),
            description: "Remind yourself of something later. It comes back to you when due, even \
                          after a restart."
                .to_owned(),
            parameters: vec![
                s("text", "What to do or think about"),
                Parameter::optional("in_minutes", "Minutes from now (default 0, max 30 days)", ParameterType::Integer),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: ScheduleArgs = parse_args!("schedule", arguments);
        let mins = args.in_minutes.min(MAX_SCHEDULE_MIN);
        let now = (self.turn.now_ms)();
        let item = Scheduled { id: format!("{now}-{}", args.text.len()), due_ms: now + mins as i64 * 60_000, text: args.text };
        match self.deps.selves.schedule(item).await {
            Ok(()) => format!("scheduled in {mins} minutes"),
            Err(e) => format!("could not schedule: {e}"),
        }
    }
}
