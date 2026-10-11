//! 她专属的 Chromium：启动（或连接）、标签页管理、页面操作。
//!
//! 每个标签页一个 flat session；"当前标签页"是大多数工具的默认目标。元素定位靠
//! `browser_snapshot` 给出的编号（页面里打上 `data-thought-ref`），也支持 CSS 选择器
//! 与坐标，三者任选。

use crate::cdp::Cdp;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// `[browser]` 配置。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct BrowserConfig {
    /// 已在运行的浏览器的调试地址（如 `http://127.0.0.1:9333`）；设了就连接，不自己启动。
    pub connect: Option<String>,
    /// 启动时用的可执行文件。
    pub executable: PathBuf,
    /// 她的 profile 目录（登录态、cookie 都在这；建议放她的工作区里）。
    pub profile_dir: PathBuf,
    /// 下载目录。
    pub download_dir: PathBuf,
    /// 启动时的调试端口（只监听 127.0.0.1）。
    pub port: u16,
    pub headless: bool,
    pub width: u32,
    pub height: u32,
    /// 额外的启动参数。
    pub args: Vec<String>,
    /// 单条 CDP 命令超时（秒）。
    pub command_timeout_secs: u64,
    /// 页面加载等待上限（秒）。
    pub load_timeout_secs: u64,
    /// 快照 / 文本输出的字符上限。
    pub max_text_chars: usize,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            connect: None,
            executable: PathBuf::from("chromium"),
            profile_dir: PathBuf::from("data/mind/workspace/.browser/profile"),
            download_dir: PathBuf::from("data/mind/workspace/downloads"),
            port: 9333,
            headless: true,
            width: 1280,
            height: 800,
            args: Vec::new(),
            command_timeout_secs: 30,
            load_timeout_secs: 30,
            max_text_chars: 20_000,
        }
    }
}

impl BrowserConfig {
    pub fn resolve_paths(&mut self, base: &std::path::Path) {
        for p in [&mut self.profile_dir, &mut self.download_dir] {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
    }
}

/// 标签页概况。
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct TabInfo {
    pub id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

pub(crate) struct State {
    /// target id → session id
    pub(crate) tabs: BTreeMap<String, String>,
    pub(crate) active: Option<String>,
}

/// 浏览器句柄。`Clone` 廉价。
#[derive(Clone)]
pub struct Browser {
    pub(crate) cdp: Cdp,
    pub(crate) state: Arc<Mutex<State>>,
    pub config: Arc<BrowserConfig>,
    /// 自己启动的进程：最后一个句柄丢弃时一并结束（kill_on_drop）。
    pub(crate) _child: Option<Arc<tokio::process::Child>>,
}

/// 选择器 / 编号 / 坐标：定位一个元素或位置。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct Target {
    /// `browser_snapshot` 给出的编号。
    #[serde(default, rename = "ref")]
    pub reference: Option<u64>,
    /// CSS 选择器。
    #[serde(default)]
    pub selector: Option<String>,
    /// 视口坐标。
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
}

impl Target {
    /// 生成在页面里找到元素的 JS 表达式。
    pub(crate) fn js(&self) -> Option<String> {
        if let Some(r) = self.reference {
            Some(format!("document.querySelector('[data-thought-ref=\"{r}\"]')"))
        } else {
            self.selector.as_ref().map(|s| format!("document.querySelector({})", json!(s)))
        }
    }
}
