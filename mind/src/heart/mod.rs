//! 驱动接口：压力完全由真实信号计算；心跳只做零 token 的判断（docs/mind-design.md §3）。

pub mod beat;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 驱动类别：自我驱动与关系驱动互相竞争；生理驱动只调制。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriveKind {
    Selfhood,
    Relation,
    Physiology,
}

/// 一拍心跳时给驱动看的信号快照（只读、廉价）。
#[derive(Debug, Clone, Default)]
pub struct Signals {
    pub now_ms: i64,
    pub self_model: crate::self_model::SelfModel,
    /// 待整理量（记忆待抽取 + 冻结关系）。
    pub untidiness: u64,
    /// 0–1+ 代谢疲劳度。
    pub fatigue: f64,
    /// 各驱动上次被满足的时间。
    pub last_satisfied_ms: BTreeMap<String, i64>,
    /// 当前在线的人。
    pub present: Vec<String>,
}

/// 驱动点火后的建议：为什么醒、想做什么。
#[derive(Debug, Clone, PartialEq)]
pub struct Urge {
    pub pressure: f32,
    /// 给她看的一句话：为什么想做这件事。
    pub reason: String,
}

/// 一次回合的结果摘要：驱动据此判断自己是否被满足。
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// 本回合她对谁说过话。
    pub said_to: Vec<String>,
    /// 本回合调用过的工具名。
    pub tools_used: Vec<String>,
    /// 本回合是否修改了自我模型。
    pub reflected: bool,
}

/// 一个驱动。实现放在 `drives/`，一个文件一个，注册即用。
pub trait Drive: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> DriveKind;

    /// 当前压力（0–1）与理由；压力为 0 时返回 `None`。
    fn urge(&self, signals: &Signals) -> Option<Urge>;

    /// 一个由它点火的回合结束后，它是否被满足（满足则记录 `last_satisfied_ms`）。
    fn satisfied(&self, outcome: &Outcome) -> bool;
}

/// 平滑饱和：`1 - exp(-x/τ)`，x、τ 同单位，τ ≤ 0 时视为 0 压力。
pub fn saturate(x: f64, tau: f64) -> f32 {
    if tau <= 0.0 || x <= 0.0 {
        return 0.0;
    }
    (1.0 - (-x / tau).exp()) as f32
}
