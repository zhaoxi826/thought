//! 工具抽象与注册表。
//!
//! 每个工具实现 [`Tool`]：自带发给模型的 JSON schema 定义，以及一段“输入原始
//! JSON 参数串、输出结果文本”的执行逻辑。[`Registry`] 按工具名统一持有与分发。

use model::Function;
use std::collections::HashMap;
use std::sync::Arc;

/// 一个可被 agent 调用的工具。
///
/// 参数解析由工具自己负责：[`call`](Tool::call) 收到的是模型回传的原始 JSON
/// 参数串，工具内部反序列化成自己的强类型参数结构。
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// 工具名，必须与 [`definition`](Tool::definition) 的 `name` 一致，用于分发。
    fn name(&self) -> &str;

    /// 发给模型的工具定义（名称、描述、参数 schema）。
    fn definition(&self) -> Function;

    /// 执行工具。`arguments` 为模型回传的原始 JSON 参数串。
    async fn call(&self, arguments: &str) -> String;
}

/// 工具注册表：按名字持有一组工具，供 agent 收集定义与分发调用。
#[derive(Default, Clone)]
pub struct Registry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个工具，以其 [`name`](Tool::name) 为键。同名会覆盖。
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool.name().to_owned(), tool);
        self
    }

    /// 全部工具的定义，用于填充请求的 `tools` 字段。
    pub fn definitions(&self) -> Vec<Function> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// 按名字分发调用；工具不存在时返回 `None`。
    pub async fn call(&self, name: &str, arguments: &str) -> Option<String> {
        match self.tools.get(name) {
            Some(tool) => Some(tool.call(arguments).await),
            None => None,
        }
    }

    /// 是否已注册某工具。
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }
}
