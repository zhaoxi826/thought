//! 截图、cookie、上传、下载列表。

use crate::browser::{Browser, Target};
use crate::cdp::{CdpError, CdpResult};
use serde_json::{json, Value};
use std::path::PathBuf;

fn err(e: impl std::fmt::Display) -> CdpError {
    CdpError(e.to_string())
}

impl Browser {
    /// 截图，保存到下载目录旁的 `screenshots/`，返回文件路径。`full` 截整页，`target` 截某元素。
    pub async fn screenshot(&self, target: Option<&Target>, full: bool) -> CdpResult<PathBuf> {
        let session = self.session().await?;
        let mut params = json!({ "format": "png", "captureBeyondViewport": full });
        if let Some(el) = target.and_then(Target::js) {
            let rect = self
                .eval(&format!(
                    "(() => {{ const e = {el}; if (!e) return null; e.scrollIntoView({{block:'center'}}); \
                     const r = e.getBoundingClientRect(); return {{x:r.left+scrollX,y:r.top+scrollY,width:r.width,height:r.height,scale:1}}; }})()"
                ))
                .await?;
            if rect.is_null() {
                return Err(err("element not found"));
            }
            params["clip"] = rect;
            params["captureBeyondViewport"] = json!(true);
        } else if full {
            let size = self.eval("({x:0,y:0,width:document.documentElement.scrollWidth,height:document.documentElement.scrollHeight,scale:1})").await?;
            params["clip"] = size;
        }
        let r = self.cdp().call(Some(&session), "Page.captureScreenshot", params).await?;
        let data = r["data"].as_str().ok_or_else(|| err("no screenshot data"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(err)?;
        let dir = self.screenshot_dir();
        std::fs::create_dir_all(&dir).map_err(err)?;
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let path = dir.join(format!("shot-{millis}.png"));
        std::fs::write(&path, bytes).map_err(err)?;
        Ok(path)
    }

    pub fn screenshot_dir(&self) -> PathBuf {
        self.config.download_dir.parent().unwrap_or(&self.config.download_dir).join("screenshots")
    }

    /// 当前页面可见的 cookie（`all=true` 时浏览器里全部的）。
    pub async fn cookies(&self, all: bool) -> CdpResult<Value> {
        let session = self.session().await?;
        let r = if all {
            self.cdp().call(Some(&session), "Network.getAllCookies", json!({})).await?
        } else {
            self.cdp().call(Some(&session), "Network.getCookies", json!({})).await?
        };
        Ok(r["cookies"].clone())
    }

    /// 设置 cookie（参数同 CDP `Network.setCookie`：name, value, domain / url, path, secure, httpOnly, expires…）。
    pub async fn set_cookie(&self, cookie: Value) -> CdpResult<()> {
        let session = self.session().await?;
        let r = self.cdp().call(Some(&session), "Network.setCookie", cookie).await?;
        if r["success"] == json!(false) {
            return Err(err("cookie rejected"));
        }
        Ok(())
    }

    /// 清除 cookie（全部）。
    pub async fn clear_cookies(&self) -> CdpResult<()> {
        let session = self.session().await?;
        self.cdp().call(Some(&session), "Network.clearBrowserCookies", json!({})).await?;
        Ok(())
    }

    /// 给 `<input type=file>` 设置要上传的文件（本地路径）。
    pub async fn upload(&self, target: &Target, files: &[String]) -> CdpResult<()> {
        let session = self.session().await?;
        let el = target.js().ok_or_else(|| err("give ref or selector of the file input"))?;
        let r = self
            .cdp()
            .call(Some(&session), "Runtime.evaluate", json!({ "expression": el }))
            .await?;
        let object_id = r["result"]["objectId"].as_str().ok_or_else(|| err("file input not found"))?;
        for f in files {
            if !std::path::Path::new(f).is_file() {
                return Err(err(format!("no such file: {f}")));
            }
        }
        self.cdp()
            .call(Some(&session), "DOM.setFileInputFiles", json!({ "files": files, "objectId": object_id }))
            .await?;
        Ok(())
    }

    /// 下载目录里的文件（最新在前）。
    pub fn downloads(&self) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64, std::time::SystemTime)> = std::fs::read_dir(&self.config.download_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file().then(|| (e.path().display().to_string(), m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
            })
            .collect();
        out.sort_by_key(|f| std::cmp::Reverse(f.2));
        out.into_iter().map(|(p, n, _)| (p, n)).collect()
    }
}
