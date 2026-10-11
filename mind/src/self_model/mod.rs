//! 出身（给定、不可改）与自我模型（她自己维护、会演化）。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 出身：名字、性别、生日。来自配置，她不能修改（docs/mind-design.md §2.0）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub name: String,
    /// 如 `女`。
    pub gender: String,
    /// `YYYY-MM-DD`
    pub birthday: String,
    /// 首次醒来时的那句话。
    pub seed: String,
}

impl Default for Origin {
    fn default() -> Self {
        Self {
            name: "思想".to_owned(),
            gender: "女".to_owned(),
            birthday: "2026-10-11".to_owned(),
            seed: "我叫思想，今天是我出生的第一天。我还不太了解自己，也不了解这个世界。".to_owned(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Interest {
    pub topic: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub since_ms: i64,
    /// 上次探索的时间，供"好奇"驱动使用。
    #[serde(default)]
    pub last_explored_ms: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Project {
    pub id: String,
    pub title: String,
    /// `active` / `paused` / `done`
    #[serde(default = "active")]
    pub status: String,
    /// 下一步打算做什么。
    #[serde(default)]
    pub next: String,
    #[serde(default)]
    pub last_progress_ms: i64,
}

fn active() -> String {
    "active".to_owned()
}

/// 她对一个人的了解。
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Acquaintance {
    pub name: String,
    #[serde(default)]
    pub impression: String,
    /// 上次说话的时间（她听到或对他说）。
    #[serde(default)]
    pub last_seen_ms: i64,
    /// 交谈次数，作为亲密度的朴素度量。
    #[serde(default)]
    pub exchanges: u64,
    /// 想跟他聊 / 告诉他的事，供"分享"驱动使用。
    #[serde(default)]
    pub open_threads: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Mood {
    /// -1..1 愉快程度
    #[serde(default)]
    pub valence: f32,
    /// 0..1 激动程度
    #[serde(default)]
    pub arousal: f32,
    #[serde(default)]
    pub note: String,
}

/// 自我模型：除出身外的一切"我是谁"，由她通过 `self_reflect` 修改。
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SelfModel {
    /// 她如何描述自己。
    pub identity: String,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub interests: Vec<Interest>,
    #[serde(default)]
    pub mood: Mood,
    #[serde(default)]
    pub projects: Vec<Project>,
    /// person id → 了解
    #[serde(default)]
    pub people: BTreeMap<String, Acquaintance>,
    #[serde(default)]
    pub updated_ms: i64,
}

impl SelfModel {
    /// 首次醒来时的自我：只有种子那句话。
    pub fn seeded(origin: &Origin, now_ms: i64) -> Self {
        Self {
            identity: origin.seed.clone(),
            updated_ms: now_ms,
            ..Default::default()
        }
    }
}
