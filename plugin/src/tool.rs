//! 工具抽象与注册表。
//!
//! 每个工具实现 [`Tool`]：自带发给模型的 JSON schema 定义，以及一段“输入原始
//! JSON 参数串、输出结果文本”的执行逻辑。[`Registry`] 按工具名统一持有与分发。

use model::Function;
use std::collections::BTreeMap;
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
///
/// 按名字有序存放，[`definitions`](Registry::definitions) 每次顺序一致，
/// 请求体字节稳定（利于 prompt 缓存）。`Clone` 只拷贝 `Arc`。
#[derive(Default, Clone)]
pub struct Registry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
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

    /// 全部工具（按名字有序）。
    pub fn tools(&self) -> impl Iterator<Item = (&str, &Arc<dyn Tool>)> {
        self.tools.iter().map(|(name, tool)| (name.as_str(), tool))
    }

    /// 已注册的工具名（有序）。
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// 并入另一个注册表的全部工具（同名覆盖）。
    pub fn extend(&mut self, other: &Registry) -> &mut Self {
        for (name, tool) in &other.tools {
            self.tools.insert(name.clone(), Arc::clone(tool));
        }
        self
    }

    /// 按名字挑出一个子集；有名字不存在时返回 `Err(那个名字)`。
    pub fn subset<S: AsRef<str>>(&self, names: &[S]) -> Result<Registry, String> {
        let mut tools = BTreeMap::new();
        for name in names {
            let name = name.as_ref();
            let tool = self.tools.get(name).ok_or_else(|| name.to_owned())?;
            tools.insert(name.to_owned(), Arc::clone(tool));
        }
        Ok(Registry { tools })
    }
}
