//! agent 层配置：主控与各类工作 agent 的定义，通常来自配置文件的 `[agent]` 段。

use serde::{Deserialize, Serialize};

/// 整个 agent 系统的配置。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AgentConfig {
    pub controller: ControllerSpec,

    /// 第二层工作 agent 的种类，主控按 `description` 选择派单对象。
    #[serde(default)]
    pub workers: Vec<WorkerSpec>,

    #[serde(default)]
    pub limits: Limits,
}

/// 第一层：主控 agent。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ControllerSpec {
    /// 引用的模型名（模型池里的 key）。
    pub model: String,

    /// 追加在内置主控提示之后的自定义提示。
    #[serde(default)]
    pub system: Option<String>,

    /// 主控额外可用的工具（如记忆检索）。主控应尽量只派单，默认不给任何真实工具。
    #[serde(default)]
    pub tools: Vec<String>,

    #[serde(default = "default_controller_turns")]
    pub max_turns: usize,
}

/// 第二层：一类工作 agent（如 coder）。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WorkerSpec {
    /// 唯一名字，主控派单时用它指定。
    pub name: String,

    /// 能力描述，展示给主控。
    pub description: String,

    pub model: String,

    pub system: String,

    /// 可用工具名（工具池里的 key）。
    #[serde(default)]
    pub tools: Vec<String>,

    /// 是否允许向下创建第三层子 agent。
    #[serde(default = "default_true")]
    pub can_spawn: bool,

    #[serde(default = "default_worker_turns")]
    pub max_turns: usize,
}

/// 全局限制，防止递归 / 并发失控。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Limits {
    /// 同时运行的工作 agent 上限（每个会话）。
    #[serde(default = "default_concurrent")]
    pub max_concurrent_tasks: usize,

    /// 子 agent 的最大轮次。
    #[serde(default = "default_worker_turns")]
    pub subagent_max_turns: usize,

    /// 单个工具结果写入历史前的最大字符数，超出截断。
    #[serde(default = "default_tool_output")]
    pub max_tool_output_chars: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_concurrent_tasks: default_concurrent(),
            subagent_max_turns: default_worker_turns(),
            max_tool_output_chars: default_tool_output(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_controller_turns() -> usize {
    8
}

fn default_worker_turns() -> usize {
    30
}

fn default_concurrent() -> usize {
    4
}

fn default_tool_output() -> usize {
    32 * 1024
}
