//! 意识流的基本单元：感知（进来的）与表达（出去的）都是流上的一个时刻。

use serde::{Deserialize, Serialize};

/// 一个人。`id` 是稳定的账号 id；`name` 用于渲染。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct Person {
    pub id: String,
    pub name: String,
}

/// 进入意识流的一条感知。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Percept {
    /// 有人对她说话。
    Heard { from: Person, text: String },

    /// 她派出的任务有了结果。`for_person` 是引发这个任务的人（如果有）。
    TaskDone {
        task_id: u64,
        worker: String,
        ok: bool,
        result: String,
        for_person: Option<String>,
    },

    /// 她给自己安排的事到期了。
    Due { text: String },

    /// 某个驱动点火：她因此醒来。
    Drive { drive: String, pressure: f32, reason: String },

    /// 某人查看了她的内心（透明是双向的）。
    Observed { by: Person, range: String },

    /// 进程启动后醒来；`slept_ms` 是距上次活动的时长；`first` 表示这是她第一次醒来。
    Awake { slept_ms: i64, first: bool },
}

impl Percept {
    /// 是否会让她醒来处理（被查看只是被感知，不单独触发一个回合）。
    pub fn triggers(&self) -> bool {
        !matches!(self, Percept::Observed { .. })
    }

    /// 是否是对外界的回应（有人说话或任务回来）；否则是她自主发起的。
    pub fn responsive(&self) -> bool {
        matches!(self, Percept::Heard { .. } | Percept::TaskDone { .. })
    }

    /// 用于回想检索的文本。
    pub fn cue(&self) -> Option<&str> {
        match self {
            Percept::Heard { text, .. } | Percept::Due { text } => Some(text),
            Percept::Drive { reason, .. } => Some(reason),
            Percept::TaskDone { result, .. } => Some(result),
            Percept::Observed { .. } | Percept::Awake { .. } => None,
        }
    }
}

/// 她发出的一次表达。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expressed {
    /// 对某人说话。
    Said { to: String, text: String },
    /// 自言自语 / 思考（不发给任何人）。
    Thought { text: String },
    /// 她用工具做了一件事（写文件、跑命令、派任务……），`summary` 是简述。
    Acted { tool: String, summary: String },
}

/// 意识流上的一个时刻。`seq` 单调递增，是流的唯一顺序。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Moment {
    pub seq: u64,
    pub at_ms: i64,
    #[serde(flatten)]
    pub body: MomentBody,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "dir", rename_all = "snake_case")]
pub enum MomentBody {
    In { percept: Percept },
    Out { expressed: Expressed },
}

impl Moment {
    pub fn percept(&self) -> Option<&Percept> {
        match &self.body {
            MomentBody::In { percept } => Some(percept),
            MomentBody::Out { .. } => None,
        }
    }

    /// 这个时刻涉及的人（他说的，或对他说的）。
    pub fn person(&self) -> Option<&str> {
        match &self.body {
            MomentBody::In { percept: Percept::Heard { from, .. } } => Some(&from.id),
            MomentBody::Out { expressed: Expressed::Said { to, .. } } => Some(to),
            _ => None,
        }
    }
}

/// 一段较早的意识流被压缩成的摘要。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Summary {
    pub from_seq: u64,
    pub to_seq: u64,
    pub from_ms: i64,
    pub to_ms: i64,
    pub text: String,
}
