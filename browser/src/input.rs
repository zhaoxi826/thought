//! 输入：点击、输入文字、按键、滚动、选择、上传。走 CDP `Input.*`，是真实事件。

use crate::browser::{Browser, Target};
use crate::cdp::{CdpError, CdpResult};
use serde_json::{json, Value};

fn err(e: impl std::fmt::Display) -> CdpError {
    CdpError(e.to_string())
}

impl Browser {
    /// 把目标解析成视口坐标（元素会先滚动到可见）。
    async fn point(&self, target: &Target) -> CdpResult<(f64, f64)> {
        if let (Some(x), Some(y)) = (target.x, target.y) {
            return Ok((x, y));
        }
        let el = target.js().ok_or_else(|| err("give ref, selector, or x and y"))?;
        let v = self
            .eval(&format!(
                "(() => {{ const e = {el}; if (!e) return null; e.scrollIntoView({{block:'center',inline:'center'}}); \
                 const r = e.getBoundingClientRect(); return [r.left + r.width/2, r.top + r.height/2]; }})()"
            ))
            .await?;
        match v.as_array().map(|a| (a[0].as_f64(), a[1].as_f64())) {
            Some((Some(x), Some(y))) => Ok((x, y)),
            _ => Err(err("element not found (take a new browser_snapshot; refs change when the page changes)")),
        }
    }

    async fn mouse(&self, kind: &str, x: f64, y: f64, button: &str, clicks: u32) -> CdpResult<Value> {
        let session = self.session().await?;
        self.cdp()
            .call(
                Some(&session),
                "Input.dispatchMouseEvent",
                json!({ "type": kind, "x": x, "y": y, "button": button, "clickCount": clicks }),
            )
            .await
    }

    /// 点击（`button`: left / right / middle；`double` 双击）。
    pub async fn click(&self, target: &Target, button: &str, double: bool) -> CdpResult<()> {
        let (x, y) = self.point(target).await?;
        self.mouse("mouseMoved", x, y, "none", 0).await?;
        let clicks = if double { 2 } else { 1 };
        self.mouse("mousePressed", x, y, button, clicks).await?;
        self.mouse("mouseReleased", x, y, button, clicks).await?;
        Ok(())
    }

    pub async fn hover(&self, target: &Target) -> CdpResult<()> {
        let (x, y) = self.point(target).await?;
        self.mouse("mouseMoved", x, y, "none", 0).await?;
        Ok(())
    }

    /// 输入文字：先聚焦目标（可选清空），再用 `Input.insertText` 写入，可选回车。
    pub async fn type_text(&self, target: &Target, text: &str, clear: bool, submit: bool) -> CdpResult<()> {
        if target.reference.is_some() || target.selector.is_some() {
            let el = target.js().expect("checked");
            let found = self
                .eval(&format!(
                    "(() => {{ const e = {el}; if (!e) return false; e.scrollIntoView({{block:'center'}}); e.focus(); \
                     if ({clear}) {{ if ('value' in e) {{ e.value=''; e.dispatchEvent(new Event('input',{{bubbles:true}})); }} \
                     else if (e.isContentEditable) e.textContent=''; }} return true; }})()"
                ))
                .await?;
            if found.as_bool() != Some(true) {
                return Err(err("element not found (take a new browser_snapshot)"));
            }
        } else if target.x.is_some() {
            self.click(target, "left", false).await?;
        }
        let session = self.session().await?;
        self.cdp().call(Some(&session), "Input.insertText", json!({ "text": text })).await?;
        if submit {
            self.press("Enter", &[]).await?;
        }
        Ok(())
    }

    /// 按键 / 组合键，如 `Enter`、`Tab`、`Escape`、`ArrowDown`、`a`；`modifiers` 取 ctrl / shift / alt / meta。
    pub async fn press(&self, key: &str, modifiers: &[String]) -> CdpResult<()> {
        let session = self.session().await?;
        let mut mask = 0;
        for m in modifiers {
            mask |= match m.to_ascii_lowercase().as_str() {
                "alt" => 1,
                "ctrl" | "control" => 2,
                "meta" | "cmd" => 4,
                "shift" => 8,
                _ => 0,
            };
        }
        let (code, vk, text) = key_info(key);
        for kind in ["keyDown", "keyUp"] {
            let mut p = json!({ "type": kind, "key": key, "code": code, "windowsVirtualKeyCode": vk, "modifiers": mask });
            if kind == "keyDown" && mask & !8 == 0
                && let Some(t) = &text
            {
                p["text"] = json!(t);
            }
            self.cdp().call(Some(&session), "Input.dispatchKeyEvent", p).await?;
        }
        Ok(())
    }

    /// 滚动：在目标位置（缺省视口中央）滚 `dx`, `dy` 像素。
    pub async fn scroll(&self, target: &Target, dx: f64, dy: f64) -> CdpResult<String> {
        let (x, y) = match self.point(target).await {
            Ok(p) => p,
            Err(_) => (f64::from(self.config.width) / 2.0, f64::from(self.config.height) / 2.0),
        };
        let session = self.session().await?;
        self.cdp()
            .call(Some(&session), "Input.dispatchMouseEvent", json!({ "type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy }))
            .await?;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let pos = self.eval("`${Math.round(scrollY)}/${Math.max(0, document.documentElement.scrollHeight - innerHeight)}`").await?;
        Ok(format!("scroll {}", pos.as_str().unwrap_or("?")))
    }
}

/// 常用键的 code / 虚拟键码 / 产生的字符。
fn key_info(key: &str) -> (String, u32, Option<String>) {
    let named = |code: &str, vk: u32, text: Option<&str>| (code.to_owned(), vk, text.map(str::to_owned));
    match key {
        "Enter" => named("Enter", 13, Some("\r")),
        "Tab" => named("Tab", 9, None),
        "Escape" => named("Escape", 27, None),
        "Backspace" => named("Backspace", 8, None),
        "Delete" => named("Delete", 46, None),
        "Space" | " " => named("Space", 32, Some(" ")),
        "ArrowUp" => named("ArrowUp", 38, None),
        "ArrowDown" => named("ArrowDown", 40, None),
        "ArrowLeft" => named("ArrowLeft", 37, None),
        "ArrowRight" => named("ArrowRight", 39, None),
        "Home" => named("Home", 36, None),
        "End" => named("End", 35, None),
        "PageUp" => named("PageUp", 33, None),
        "PageDown" => named("PageDown", 34, None),
        k if k.chars().count() == 1 => {
            let c = k.chars().next().expect("one char");
            let up = c.to_ascii_uppercase();
            let code = if c.is_ascii_alphabetic() { format!("Key{up}") } else if c.is_ascii_digit() { format!("Digit{c}") } else { String::new() };
            (code, up as u32, Some(k.to_owned()))
        }
        k => (k.to_owned(), 0, None),
    }
}
