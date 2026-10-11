//! 交互工具：点击、悬停、输入、按键、滚动、选择、上传、等待。

use super::{after, opt, s, str_arg, target, target_params, Maker};
use model::{Parameter, ParameterType as T};
use plugin::Tool;
use std::sync::Arc;

fn with(mut extra: Vec<Parameter>) -> Vec<Parameter> {
    let mut p = target_params();
    p.append(&mut extra);
    p
}

pub(super) fn tools(mk: &Maker) -> Vec<Arc<dyn Tool>> {
    vec![
        mk("browser_click", "Click an element with real mouse events. Returns a fresh snapshot.",
            with(vec![opt("button", "left (default), right or middle", T::String), opt("double", "Double click", T::Boolean)]),
            |b, a| Box::pin(async move {
                let button = a["button"].as_str().unwrap_or("left").to_owned();
                match b.click(&target(&a), &button, a["double"].as_bool().unwrap_or(false)).await {
                    Ok(()) => {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        after(&b, "clicked".into()).await
                    }
                    Err(e) => format!("click failed: {e}"),
                }
            })),
        mk("browser_type", "Type text into an input (focus it first; optionally clear it and press Enter after).",
            with(vec![s("text", "Text to type"), opt("clear", "Clear existing value first", T::Boolean), opt("submit", "Press Enter afterwards", T::Boolean)]),
            |b, a| Box::pin(async move {
                let r = b.type_text(&target(&a), str_arg(&a, "text"), a["clear"].as_bool().unwrap_or(false), a["submit"].as_bool().unwrap_or(false)).await;
                match r {
                    Ok(()) => {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        after(&b, "typed".into()).await
                    }
                    Err(e) => format!("type failed: {e}"),
                }
            })),
        mk("browser_press", "Press a key or shortcut on the focused element, e.g. Enter, Tab, Escape, ArrowDown, a (with modifiers ctrl/shift/alt/meta).",
            vec![s("key", "Key name"), opt("modifiers", "Array of ctrl, shift, alt, meta", T::Array)],
            |b, a| Box::pin(async move {
                let mods: Vec<String> = serde_json::from_value(a["modifiers"].clone()).unwrap_or_default();
                match b.press(str_arg(&a, "key"), &mods).await {
                    Ok(()) => {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        after(&b, "pressed".into()).await
                    }
                    Err(e) => format!("press failed: {e}"),
                }
            })),
        mk("browser_hover", "Move the mouse over an element.",
            target_params(),
            |b, a| Box::pin(async move {
                match b.hover(&target(&a)).await {
                    Ok(()) => after(&b, "hovering".into()).await,
                    Err(e) => format!("hover failed: {e}"),
                }
            })),
        mk("browser_scroll", "Scroll the page (or the element under ref/selector) by dx/dy pixels; positive dy scrolls down.",
            with(vec![opt("dx", "Horizontal pixels", T::Number), opt("dy", "Vertical pixels (default 600)", T::Number)]),
            |b, a| Box::pin(async move {
                let (dx, dy) = (a["dx"].as_f64().unwrap_or(0.0), a["dy"].as_f64().unwrap_or(600.0));
                match b.scroll(&target(&a), dx, dy).await {
                    Ok(pos) => after(&b, pos).await,
                    Err(e) => format!("scroll failed: {e}"),
                }
            })),
        mk("browser_select", "Choose an option in a <select> by its value or visible text.",
            with(vec![s("value", "Option value or visible text")]),
            |b, a| Box::pin(async move {
                let Some(el) = target(&a).js() else { return "give ref or selector".into() };
                let v = serde_json::json!(str_arg(&a, "value"));
                let js = format!(
                    "(() => {{ const e = {el}; if (!e) return 'not found'; const o = [...e.options].find(o => o.value === {v} || o.text.trim() === {v}); \
                     if (!o) return 'no such option'; e.value = o.value; e.dispatchEvent(new Event('input',{{bubbles:true}})); \
                     e.dispatchEvent(new Event('change',{{bubbles:true}})); return 'selected ' + o.text; }})()"
                );
                match b.eval(&js).await {
                    Ok(r) => after(&b, r.as_str().unwrap_or("?").to_owned()).await,
                    Err(e) => format!("select failed: {e}"),
                }
            })),
        mk("browser_upload", "Attach local files (absolute paths, e.g. in your workspace) to a file input.",
            with(vec![Parameter::required("files", "Array of absolute file paths", T::Array)]),
            |b, a| Box::pin(async move {
                let files: Vec<String> = serde_json::from_value(a["files"].clone()).unwrap_or_default();
                match b.upload(&target(&a), &files).await {
                    Ok(()) => after(&b, format!("attached {} file(s)", files.len())).await,
                    Err(e) => format!("upload failed: {e}"),
                }
            })),
        mk("browser_wait", "Wait for a CSS selector to appear, for text to appear on the page, or for some milliseconds.",
            vec![opt("selector", "CSS selector", T::String), opt("text", "Text that should appear", T::String), opt("ms", "Milliseconds to wait first (max 60000)", T::Integer)],
            |b, a| Box::pin(async move {
                match b.wait(a["selector"].as_str(), a["text"].as_str(), a["ms"].as_u64()).await {
                    Ok(r) => after(&b, r).await,
                    Err(e) => format!("wait: {e}"),
                }
            })),
    ]
}
