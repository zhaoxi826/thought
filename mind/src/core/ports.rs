//! 心智与外界之间的接口（trait）。心智核心只依赖这些，不知道 HTTP、SurrealDB、
//! 具体模型的存在；每个实现都能独立替换、升级（docs/mind-design.md §5）。

use crate::core::stream::{Expressed, Moment, Person, Summary};
use crate::self_model::SelfModel;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 端口错误：只带一段说明，由调用方决定记日志还是返回给模型。
#[derive(Debug, Clone)]
pub struct PortError(pub String);

impl std::fmt::Display for PortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PortError {}

pub type PortResult<T> = Result<T, PortError>;

// ---- 意识流的持久化 ----

/// 意识流存储：追加时刻、按区间读取、保存压缩摘要。
#[async_trait]
pub trait StreamStore: Send + Sync {
    /// 追加一个时刻；由存储分配 `seq`（单调递增），返回完整时刻。
    async fn append(&self, at_ms: i64, body: crate::core::stream::MomentBody) -> PortResult<Moment>;

    /// `seq > after` 的时刻，按 seq 升序，至多 `limit` 条。
    async fn since(&self, after: u64, limit: usize) -> PortResult<Vec<Moment>>;

    /// 最近的 `limit` 条，按 seq 升序。
    async fn latest(&self, limit: usize) -> PortResult<Vec<Moment>>;

    /// 与某人有关的时刻（他说的、对他说的），`seq < before`，按 seq 降序，至多 `limit` 条。
    async fn with_person(&self, person_id: &str, before: u64, limit: usize) -> PortResult<Vec<Moment>>;

    /// 保存一段摘要。
    async fn save_summary(&self, summary: Summary) -> PortResult<()>;

    /// 最近的 `limit` 段摘要，按时间升序。
    async fn summaries(&self, limit: usize) -> PortResult<Vec<Summary>>;

    /// 已被摘要覆盖到的最大 seq（工作记忆从它之后开始）。
    async fn summarized_through(&self) -> PortResult<u64>;
}

// ---- 自我 ----

/// 自我模型的一次修改记录（她的成长轨迹）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SelfChange {
    pub at_ms: i64,
    /// 修改了哪个字段，如 `identity`、`interests`、`people.<id>`。
    pub field: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: String,
}

/// 持久化的待办（取代只在内存里的 `add_todo`，重启不丢）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Scheduled {
    pub id: String,
    pub due_ms: i64,
    pub text: String,
}

/// 自我存储：自我模型、它的变化历史、日记、待办。
#[async_trait]
pub trait SelfStore: Send + Sync {
    /// 读取当前自我模型；首次启动时返回 `None`（由心智用种子初始化）。
    async fn load(&self) -> PortResult<Option<SelfModel>>;

    /// 保存自我模型并记录变化。
    async fn save(&self, model: &SelfModel, change: Option<SelfChange>) -> PortResult<()>;

    async fn history(&self, limit: usize) -> PortResult<Vec<SelfChange>>;

    async fn write_journal(&self, at_ms: i64, text: &str) -> PortResult<()>;

    async fn schedule(&self, item: Scheduled) -> PortResult<()>;

    /// 取出所有 `due_ms <= now` 的待办并删除。
    async fn take_due(&self, now_ms: i64) -> PortResult<Vec<Scheduled>>;
}

// ---- 长期记忆 ----

/// 注入上下文的一条回想。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Recollection {
    pub id: String,
    pub content: String,
    pub about: Vec<String>,
    /// `private` / `shared` / `public`
    pub visibility: String,
    pub score: f64,
}

/// 长期记忆端口。检索不做隐私过滤：她知道一切，自己判断说不说。
#[async_trait]
pub trait MemoryPort: Send + Sync {
    async fn recall(&self, query: &str, limit: usize) -> PortResult<Vec<Recollection>>;

    /// 写入一条记忆（压缩时沉淀要点、日记等）。返回记忆 id。
    async fn remember(&self, content: &str, about: &[String], source: &str) -> PortResult<String>;

    /// 待整理量（待抽取段落 + 冻结关系），供"整理"驱动使用。
    async fn untidiness(&self) -> PortResult<u64>;
}

// ---- 表达 ----

/// 把她的话送到对方那里。网页、MCP 推送等各是一个实现。
#[async_trait]
pub trait Expression: Send + Sync {
    /// `moment` 是已写入意识流的那次表达（带 seq / 时间），实现负责投递。
    async fn deliver(&self, moment: &Moment, expressed: &Expressed) -> PortResult<()>;

    /// 此人当前是否在线（有打开的连接）。
    fn is_present(&self, person_id: &str) -> bool;
}

// ---- 预算 / 代谢 ----

/// 代谢状态：驱动据此调整阈值，心智据此决定能否自主醒来。
pub trait Metabolism: Send + Sync {
    /// 0–1+：今日自主用量 / 软额度。
    fn fatigue(&self) -> f64;

    /// 未急停且未超硬上限。
    fn may_act_autonomously(&self) -> bool;
}

// ---- 审计 ----

/// 隐私审计（只记录，不拦截）与内心查看记录。
#[async_trait]
pub trait Auditor: Send + Sync {
    /// 她对 `to` 说话时，本回合检索命中过涉及其他人的 private 记忆。
    async fn disclosure(&self, at_ms: i64, to: &str, memory_ids: &[String], text: &str);
}

// ---- 人 ----

/// 认识的人（名字、是否 admin 等），用于渲染与驱动。
#[async_trait]
pub trait People: Send + Sync {
    async fn get(&self, id: &str) -> PortResult<Option<Person>>;
    async fn all(&self) -> PortResult<Vec<Person>>;
}

// ---- 认知 ----

/// 执行一个回合：给定完整上下文（system + 渲染后的意识流）与工具，跑到她不再调用工具为止。
/// 默认实现包装 `agent::Runner`；测试可以替换成脚本。
#[async_trait]
pub trait Cognition: Send + Sync {
    /// 返回本回合最后一段自由文本（回合内对人说话一律走 `say` 工具，这段文本视为她的思考）。
    async fn think(&self, history: &mut Vec<model::Message>, tools: &plugin::Registry) -> PortResult<String>;
}

/// 时钟（测试可注入）。
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// 系统时钟。
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}
