//! 导航与标签页工具。

use super::{after, opt, s, str_arg, Maker};
use model::ParameterType as T;
use plugin::Tool;
use std::sync::Arc;

pub(super) fn tools(mk: &Maker) -> Vec<Arc<dyn Tool>> {
    vec![
        mk("browser_navigate", "Open a URL in the current tab and wait for it to load. Returns a snapshot of the page.",
            vec![s("url", "Absolute URL")],
            |b, a| Box::pin(async move {
                match b.navigate(str_arg(&a, "url")).await {
                    Ok(url) => after(&b, format!("opened {url}")).await,
                    Err(e) => format!("navigate failed: {e}"),
                }
            })),
        mk("browser_history", "Go back, forward, or reload the current tab.",
            vec![s("action", "back, forward or reload")],
            |b, a| Box::pin(async move {
                match b.history(str_arg(&a, "action")).await {
                    Ok(url) => after(&b, format!("now at {url}")).await,
                    Err(e) => format!("history failed: {e}"),
                }
            })),
        mk("browser_tabs", "List open tabs (id, url, title, which is current).",
            vec![],
            |b, _| Box::pin(async move {
                match b.tabs().await {
                    Ok(tabs) => tabs.iter().map(|t| format!("{} {} — {} ({})", if t.active { "*" } else { " " }, t.id, t.title, t.url)).collect::<Vec<_>>().join("\n"),
                    Err(e) => format!("tabs failed: {e}"),
                }
            })),
        mk("browser_tab_new", "Open a new tab and switch to it.",
            vec![opt("url", "URL to open (default about:blank)", T::String)],
            |b, a| Box::pin(async move {
                let url = a["url"].as_str().unwrap_or("about:blank").to_owned();
                match b.new_tab(&url).await {
                    Ok(id) => after(&b, format!("new tab {id}")).await,
                    Err(e) => format!("new tab failed: {e}"),
                }
            })),
        mk("browser_tab_switch", "Switch to another tab by id (see browser_tabs).",
            vec![s("id", "Tab id")],
            |b, a| Box::pin(async move {
                match b.switch_tab(str_arg(&a, "id")).await {
                    Ok(()) => after(&b, "switched".into()).await,
                    Err(e) => format!("switch failed: {e}"),
                }
            })),
        mk("browser_tab_close", "Close a tab by id (default: the current tab).",
            vec![opt("id", "Tab id", T::String)],
            |b, a| Box::pin(async move {
                let id = match a["id"].as_str() {
                    Some(id) => id.to_owned(),
                    None => match b.tabs().await.ok().and_then(|t| t.into_iter().find(|t| t.active)) {
                        Some(t) => t.id,
                        None => return "no tab to close".into(),
                    },
                };
                match b.close_tab(&id).await {
                    Ok(()) => format!("closed {id}"),
                    Err(e) => format!("close failed: {e}"),
                }
            })),
    ]
}
