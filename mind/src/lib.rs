//! 思想的心智：单一自我、单一意识流。
//!
//! - [`core`]：意识流的类型（感知 / 表达 / 时刻 / 摘要）与外部端口（trait）；
//! - [`self_model`]：出身（不可改）与自我模型（她自己维护）；
//! - [`heart`]：驱动接口与信号；
//!
//! 心智核心只依赖 `core::ports` 里的 trait，存储、表达通道、记忆、预算都由宿主（server）注入。
//! 设计见 `docs/mind-design.md`。

pub mod core;
pub mod heart;
pub mod self_model;
pub mod tools;

pub use core::mind::{person, Mind, MindConfig, MindEvent, Ports};
pub use core::ports::{
    Auditor, Clock, Cognition, Expression, MemoryPort, Metabolism, People, PortError, PortResult,
    Recollection, Scheduled, SelfChange, SelfStore, StreamStore, SystemClock, TurnKind,
};
pub use core::stream::{Expressed, Moment, MomentBody, Percept, Person, Summary};
pub use heart::{Drive, DriveKind, Outcome, Signals, Urge};
pub use self_model::{Acquaintance, Interest, Mood, Origin, Project, SelfModel};
