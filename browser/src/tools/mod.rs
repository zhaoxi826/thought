//! `browser_*` 工具：把 [`Browser`] 的能力交给 agent。
//!
//! 定位元素三选一：`ref`（`browser_snapshot` 的编号，推荐）、`selector`（CSS）、`x`+`y`（视口坐标）。
//! 没封装到的能力用 `browser_cdp` 发任意 CDP 命令。

use crate::browser::{Browser, Target};
use model::{Function, Parameter, ParameterType};
use plugin::{Registry, Tool};
use serde_json::Value;
use std::sync::Arc;

/// 注册全部浏览器工具。
pub fn register_browser_tools(registry: &mut Registry, browser: Browser) {
    let b = Arc::new(browser);
    for tool in tools(&b) {
        registry.register(tool);
    }
}

pub(crate) type Handler = fn(Arc<Browser>, Value) -> futures::future::BoxFuture<'static, String>;

struct BrowserTool {
    name: &'static str,
    description: &'static str,
    params: Vec<Parameter>,
    browser: Arc<Browser>,
    handler: Handler,
}

#[async_trait::async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        self.name
    }

    fn definition(&self) -> Function {
        Function { name: self.name.to_owned(), description: self.description.to_owned(), parameters: self.params.clone(), strict: false }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: Value = match serde_json::from_str(if arguments.trim().is_empty() { "{}" } else { arguments }) {
            Ok(v) => v,
            Err(e) => return format!("invalid arguments for {}: {e}", self.name),
        };
        if let Err(e) = self.browser.ensure_alive().await {
            return format!("the browser is down and could not be restarted: {e}");
        }
        (self.handler)(Arc::clone(&self.browser), args).await
    }
}

pub(crate) fn s(name: &str, desc: &str) -> Parameter {
    Parameter::required(name, desc, ParameterType::String)
}
pub(crate) fn opt(name: &str, desc: &str, t: ParameterType) -> Parameter {
    Parameter::optional(name, desc, t)
}
pub(crate) fn target_params() -> Vec<Parameter> {
    vec![
        opt("ref", "Element number from browser_snapshot", ParameterType::Integer),
        opt("selector", "CSS selector (alternative to ref)", ParameterType::String),
        opt("x", "Viewport x (with y, alternative to ref)", ParameterType::Number),
        opt("y", "Viewport y", ParameterType::Number),
    ]
}
pub(crate) fn target(args: &Value) -> Target {
    serde_json::from_value(args.clone()).unwrap_or_default()
}
pub(crate) fn str_arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args[k].as_str().unwrap_or("")
}

/// 动作之后附上新快照，省一次调用。
pub(crate) async fn after(b: &Browser, what: String) -> String {
    match b.snapshot().await {
        Ok(snap) => format!("{what}\n\n{}", snap.render()),
        Err(e) => format!("{what}\n(snapshot failed: {e})"),
    }
}

fn tools(b: &Arc<Browser>) -> Vec<Arc<dyn Tool>> {
    let mk = |name, description, params, handler: Handler| -> Arc<dyn Tool> {
        Arc::new(BrowserTool { name, description, params, browser: Arc::clone(b), handler })
    };
    let mut list = nav::tools(&mk);
    list.extend(interact::tools(&mk));
    list.extend(inspect::tools(&mk));
    list
}

mod inspect;
mod interact;
mod nav;

pub(crate) type Maker<'a> = dyn Fn(&'static str, &'static str, Vec<Parameter>, Handler) -> Arc<dyn Tool> + 'a;
