//! 心智测试共享件：内存版端口 + 脚本化认知。

#![allow(dead_code)]

use async_trait::async_trait;
use mind::core::stream::MomentBody;
use mind::*;
use model::{Message, ToolCall};
use plugin::Registry;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

// ---- 时钟 ----

pub struct FakeClock(pub AtomicI64);

impl FakeClock {
    pub fn new(ms: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(ms)))
    }
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::Relaxed);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

// ---- 意识流 ----

#[derive(Default)]
pub struct MemStream {
    pub moments: Mutex<Vec<Moment>>,
    pub summaries: Mutex<Vec<Summary>>,
    pub attended: Mutex<u64>,
}

#[async_trait]
impl StreamStore for MemStream {
    async fn append(&self, at_ms: i64, body: MomentBody) -> PortResult<Moment> {
        let mut m = self.moments.lock().unwrap();
        let moment = Moment { seq: m.len() as u64 + 1, at_ms, body };
        m.push(moment.clone());
        Ok(moment)
    }
    async fn since(&self, after: u64, limit: usize) -> PortResult<Vec<Moment>> {
        Ok(self.moments.lock().unwrap().iter().filter(|m| m.seq > after).take(limit).cloned().collect())
    }
    async fn latest(&self, limit: usize) -> PortResult<Vec<Moment>> {
        let m = self.moments.lock().unwrap();
        Ok(m[m.len().saturating_sub(limit)..].to_vec())
    }
    async fn with_person(&self, person_id: &str, before: u64, limit: usize) -> PortResult<Vec<Moment>> {
        Ok(self
            .moments
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|m| m.seq < before && m.person() == Some(person_id))
            .take(limit)
            .cloned()
            .collect())
    }
    async fn save_summary(&self, summary: Summary) -> PortResult<()> {
        self.summaries.lock().unwrap().push(summary);
        Ok(())
    }
    async fn summaries(&self, limit: usize) -> PortResult<Vec<Summary>> {
        let s = self.summaries.lock().unwrap();
        Ok(s[s.len().saturating_sub(limit)..].to_vec())
    }
    async fn summarized_through(&self) -> PortResult<u64> {
        Ok(self.summaries.lock().unwrap().last().map_or(0, |s| s.to_seq))
    }
    async fn attended_through(&self) -> PortResult<u64> {
        Ok(*self.attended.lock().unwrap())
    }
    async fn mark_attended(&self, seq: u64) -> PortResult<()> {
        *self.attended.lock().unwrap() = seq;
        Ok(())
    }
}

// ---- 自我 ----

#[derive(Default)]
pub struct MemSelf {
    pub model: Mutex<Option<SelfModel>>,
    pub changes: Mutex<Vec<SelfChange>>,
    pub journal: Mutex<Vec<String>>,
    pub scheduled: Mutex<Vec<Scheduled>>,
}

#[async_trait]
impl SelfStore for MemSelf {
    async fn load(&self) -> PortResult<Option<SelfModel>> {
        Ok(self.model.lock().unwrap().clone())
    }
    async fn save(&self, model: &SelfModel, change: Option<SelfChange>) -> PortResult<()> {
        *self.model.lock().unwrap() = Some(model.clone());
        if let Some(c) = change {
            self.changes.lock().unwrap().push(c);
        }
        Ok(())
    }
    async fn history(&self, limit: usize) -> PortResult<Vec<SelfChange>> {
        Ok(self.changes.lock().unwrap().iter().rev().take(limit).cloned().collect())
    }
    async fn write_journal(&self, _at_ms: i64, text: &str) -> PortResult<()> {
        self.journal.lock().unwrap().push(text.to_owned());
        Ok(())
    }
    async fn schedule(&self, item: Scheduled) -> PortResult<()> {
        self.scheduled.lock().unwrap().push(item);
        Ok(())
    }
    async fn take_due(&self, now_ms: i64) -> PortResult<Vec<Scheduled>> {
        let mut s = self.scheduled.lock().unwrap();
        let (due, rest): (Vec<_>, Vec<_>) = s.drain(..).partition(|i| i.due_ms <= now_ms);
        *s = rest;
        Ok(due)
    }
}

// ---- 记忆 ----

#[derive(Default)]
pub struct MemMemory {
    pub items: Mutex<Vec<Recollection>>,
    pub untidy: Mutex<u64>,
}

#[async_trait]
impl MemoryPort for MemMemory {
    async fn recall(&self, query: &str, limit: usize) -> PortResult<Vec<Recollection>> {
        Ok(self
            .items
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.content.chars().any(|c| query.contains(c) && !c.is_whitespace()))
            .take(limit)
            .cloned()
            .collect())
    }
    async fn remember(&self, content: &str, about: &[String], visibility: &str, _source: &str) -> PortResult<String> {
        let mut items = self.items.lock().unwrap();
        let id = format!("m{}", items.len() + 1);
        items.push(Recollection { id: id.clone(), content: content.into(), about: about.to_vec(), visibility: visibility.into(), score: 1.0 });
        Ok(id)
    }
    async fn untidiness(&self) -> PortResult<u64> {
        Ok(*self.untidy.lock().unwrap())
    }
}

// ---- 表达 / 代谢 / 审计 / 人 ----

#[derive(Default)]
pub struct MemExpression {
    pub delivered: Mutex<Vec<(String, String)>>,
    pub present: Mutex<Vec<String>>,
}

#[async_trait]
impl Expression for MemExpression {
    async fn deliver(&self, _moment: &Moment, expressed: &Expressed) -> PortResult<()> {
        if let Expressed::Said { to, text } = expressed {
            self.delivered.lock().unwrap().push((to.clone(), text.clone()));
        }
        Ok(())
    }
    fn is_present(&self, person_id: &str) -> bool {
        self.present.lock().unwrap().iter().any(|p| p == person_id)
    }
}

pub struct FakeMetabolism(pub Mutex<(f64, bool)>);

impl Metabolism for FakeMetabolism {
    fn fatigue(&self) -> f64 {
        self.0.lock().unwrap().0
    }
    fn may_act_autonomously(&self) -> bool {
        self.0.lock().unwrap().1
    }
}

#[derive(Default)]
pub struct MemAuditor(pub Mutex<Vec<(String, Vec<String>, String)>>);

#[async_trait]
impl Auditor for MemAuditor {
    async fn disclosure(&self, _at_ms: i64, to: &str, memory_ids: &[String], text: &str) {
        self.0.lock().unwrap().push((to.into(), memory_ids.to_vec(), text.into()));
    }
}

pub struct MemPeople(pub Vec<Person>);

#[async_trait]
impl People for MemPeople {
    async fn get(&self, id: &str) -> PortResult<Option<Person>> {
        Ok(self.0.iter().find(|p| p.id == id).cloned())
    }
    async fn all(&self) -> PortResult<Vec<Person>> {
        Ok(self.0.clone())
    }
}

// ---- 脚本化认知 ----

/// 每个回合依次弹出一个"剧本"：先调用若干工具，再返回心里话。
/// 同时记录每个回合看到的 system 与 user 消息，供断言。
#[derive(Default)]
pub struct Script {
    pub turns: Mutex<VecDeque<Turn>>,
    pub seen: Mutex<Vec<(TurnKind, String, String)>>,
}

pub struct Turn {
    pub calls: Vec<(&'static str, serde_json::Value)>,
    pub thought: &'static str,
    pub fail: bool,
}

impl Turn {
    pub fn say(to: &str, text: &str) -> Self {
        Self::calls(vec![("say", serde_json::json!({"to": to, "text": text}))], "")
    }
    pub fn calls(calls: Vec<(&'static str, serde_json::Value)>, thought: &'static str) -> Self {
        Self { calls, thought, fail: false }
    }
    pub fn thought(thought: &'static str) -> Self {
        Self::calls(vec![], thought)
    }
    pub fn fail() -> Self {
        Self { calls: vec![], thought: "", fail: true }
    }
}

impl Script {
    pub fn new(turns: Vec<Turn>) -> Arc<Self> {
        Arc::new(Self { turns: Mutex::new(turns.into()), seen: Mutex::default() })
    }
}

#[async_trait]
impl Cognition for Script {
    async fn think(&self, kind: TurnKind, history: &mut Vec<Message>, tools: &Registry) -> PortResult<String> {
        self.seen.lock().unwrap().push((kind, history[0].text().to_owned(), history[1].text().to_owned()));
        let turn = self.turns.lock().unwrap().pop_front().unwrap_or_else(|| Turn::thought(""));
        let calls: Vec<ToolCall> = turn
            .calls
            .iter()
            .enumerate()
            .map(|(i, (name, args))| ToolCall { id: format!("c{i}"), name: (*name).into(), arguments: args.to_string() })
            .collect();
        if !calls.is_empty() {
            history.push(Message::Assistant { content: None, reasoning: None, tool_calls: calls.clone() });
            for c in &calls {
                let out = tools.call(&c.name, &c.arguments).await.unwrap_or_else(|| format!("unknown tool {}", c.name));
                history.push(Message::tool(&c.id, out));
            }
        }
        // 失败发生在工具调用之后（模拟"说到一半出错"）
        if turn.fail {
            return Err(PortError("scripted failure".into()));
        }
        history.push(Message::assistant(turn.thought));
        Ok(turn.thought.to_owned())
    }
}

// ---- 组装 ----

pub struct World {
    pub clock: Arc<FakeClock>,
    pub stream: Arc<MemStream>,
    pub selves: Arc<MemSelf>,
    pub memory: Arc<MemMemory>,
    pub expression: Arc<MemExpression>,
    pub metabolism: Arc<FakeMetabolism>,
    pub auditor: Arc<MemAuditor>,
    pub script: Arc<Script>,
}

/// 2026-10-11 14:00 UTC+8
pub const BIRTH_AFTERNOON: i64 = 1_791_676_800_000 + 6 * 3_600_000;

pub fn people() -> Vec<Person> {
    vec![person("u1", "zhaoxi"), person("u2", "小王")]
}

impl World {
    pub fn new(turns: Vec<Turn>) -> Self {
        Self {
            clock: FakeClock::new(BIRTH_AFTERNOON),
            stream: Arc::default(),
            selves: Arc::default(),
            memory: Arc::default(),
            expression: Arc::default(),
            metabolism: Arc::new(FakeMetabolism(Mutex::new((0.0, true)))),
            auditor: Arc::default(),
            script: Script::new(turns),
        }
    }

    pub fn ports(&self) -> Ports {
        Ports {
            stream: self.stream.clone(),
            selves: self.selves.clone(),
            memory: Some(self.memory.clone()),
            expression: self.expression.clone(),
            metabolism: self.metabolism.clone(),
            auditor: self.auditor.clone(),
            people: Arc::new(MemPeople(people())),
            cognition: self.script.clone(),
            clock: self.clock.clone(),
            tools: Registry::new(),
            summarizer: None,
        }
    }

    pub async fn start(&self, config: MindConfig) -> Mind {
        Mind::start(config, self.ports()).await.unwrap()
    }

    /// 等心智处理完积压：连续一段时间不在思考，且意识流不再增长。
    pub async fn settle(&self, mind: &Mind) {
        let mut quiet = 0;
        let mut last_len = usize::MAX;
        for _ in 0..400 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let len = self.stream.moments.lock().unwrap().len();
            if !mind.is_thinking() && len == last_len {
                quiet += 1;
                if quiet >= 5 {
                    return;
                }
            } else {
                quiet = 0;
            }
            last_len = len;
        }
        panic!("mind did not settle");
    }

    pub fn delivered(&self) -> Vec<(String, String)> {
        self.expression.delivered.lock().unwrap().clone()
    }

    pub fn bodies(&self) -> Vec<MomentBody> {
        self.stream.moments.lock().unwrap().iter().map(|m| m.body.clone()).collect()
    }
}

pub fn heard(id: &str, name: &str, text: &str) -> Percept {
    Percept::Heard { from: person(id, name), text: text.into() }
}
