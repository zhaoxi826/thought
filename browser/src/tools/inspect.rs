//! 观察与底层工具：快照、文本、JS、截图、cookie、下载、原始 CDP。

use super::{opt, s, str_arg, target, target_params, Maker};
use model::ParameterType as T;
use plugin::Tool;
use std::sync::Arc;

pub(super) fn tools(mk: &Maker) -> Vec<Arc<dyn Tool>> {
    vec![
        mk("browser_snapshot", "Read the current page: title, url, headings, text, and numbered interactive elements [n] you can target with ref=n.",
            vec![],
            |b, _| Box::pin(async move {
                match b.snapshot().await {
                    Ok(snap) => snap.render(),
                    Err(e) => format!("snapshot failed: {e}"),
                }
            })),
        mk("browser_text", "Get the full visible text of the page (or of one element).",
            target_params(),
            |b, a| Box::pin(async move {
                let el = target(&a).js().unwrap_or_else(|| "document.body".into());
                let max = b.config.max_text_chars;
                match b.eval(&format!("(() => {{ const e = {el}; return e ? e.innerText : null; }})()")).await {
                    Ok(v) => match v.as_str() {
                        Some(t) if t.chars().count() > max => format!("{}…[truncated]", t.chars().take(max).collect::<String>()),
                        Some(t) => t.to_owned(),
                        None => "element not found".into(),
                    },
                    Err(e) => format!("text failed: {e}"),
                }
            })),
        mk("browser_eval", "Run JavaScript in the page and return the JSON result. Promises are awaited. Full access to the page.",
            vec![s("script", "A JS expression, e.g. document.title or (async () => { ... })()")],
            |b, a| Box::pin(async move {
                match b.eval(str_arg(&a, "script")).await {
                    Ok(v) => {
                        let text = if v.is_string() { v.as_str().unwrap_or("").to_owned() } else { v.to_string() };
                        let max = b.config.max_text_chars;
                        if text.chars().count() > max { format!("{}…[truncated]", text.chars().take(max).collect::<String>()) } else { text }
                    }
                    Err(e) => format!("eval failed: {e}"),
                }
            })),
        mk("browser_screenshot", "Take a PNG screenshot (viewport, full page, or one element). Saves it in your workspace and returns the path.",
            { let mut p = target_params(); p.push(opt("full", "Capture the whole page", T::Boolean)); p },
            |b, a| Box::pin(async move {
                let t = target(&a);
                let el = (t.reference.is_some() || t.selector.is_some()).then_some(&t);
                match b.screenshot(el, a["full"].as_bool().unwrap_or(false)).await {
                    Ok(path) => format!("saved {}", path.display()),
                    Err(e) => format!("screenshot failed: {e}"),
                }
            })),
        mk("browser_cookies", "Read, set or clear cookies. action=get (current page; all=true for every site), set (cookie object as in CDP Network.setCookie), or clear (all cookies).",
            vec![s("action", "get, set or clear"), opt("all", "With get: every site", T::Boolean), opt("cookie", "With set: {name, value, domain or url, path, secure, httpOnly, expires}", T::Object)],
            |b, a| Box::pin(async move {
                match str_arg(&a, "action") {
                    "get" => match b.cookies(a["all"].as_bool().unwrap_or(false)).await {
                        Ok(c) => c.to_string(),
                        Err(e) => format!("cookies failed: {e}"),
                    },
                    "set" => match b.set_cookie(a["cookie"].clone()).await {
                        Ok(()) => "cookie set".into(),
                        Err(e) => format!("set cookie failed: {e}"),
                    },
                    "clear" => match b.clear_cookies().await {
                        Ok(()) => "all cookies cleared".into(),
                        Err(e) => format!("clear failed: {e}"),
                    },
                    other => format!("unknown action {other:?}"),
                }
            })),
        mk("browser_downloads", "List files the browser downloaded (in your workspace).",
            vec![],
            |b, _| Box::pin(async move {
                let files = b.downloads();
                if files.is_empty() { return "no downloads yet".into() }
                files.iter().take(50).map(|(p, n)| format!("{p} ({n} bytes)")).collect::<Vec<_>>().join("\n")
            })),
        mk("browser_cdp", "Send any Chrome DevTools Protocol command, for anything the other browser_* tools don't cover. target=page sends it to the current tab (default), target=browser to the browser.",
            vec![s("method", "e.g. Network.setExtraHTTPHeaders, Emulation.setGeolocationOverride, Page.printToPDF"), opt("params", "Command parameters", T::Object), opt("target", "page (default) or browser", T::String)],
            |b, a| Box::pin(async move {
                let params = if a["params"].is_object() { a["params"].clone() } else { serde_json::json!({}) };
                let session = if a["target"].as_str() == Some("browser") { None } else {
                    match b.session().await { Ok(s) => Some(s), Err(e) => return format!("cdp failed: {e}") }
                };
                match b.cdp().call(session.as_deref(), str_arg(&a, "method"), params).await {
                    Ok(v) => {
                        let text = v.to_string();
                        let max = b.config.max_text_chars;
                        if text.len() > max { format!("{}…[truncated]", text.chars().take(max).collect::<String>()) } else { text }
                    }
                    Err(e) => format!("cdp failed: {e}"),
                }
            })),
    ]
}
