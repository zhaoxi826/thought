//! 思想专属的 Chromium，经 CDP 完全控制。
//!
//! - [`Browser`]：启动她自己的 Chromium（profile、下载目录在她的工作区）或连接已有实例；
//!   多标签页、导航、快照、真实鼠标 / 键盘事件、JS、截图、cookie、上传、下载；
//! - [`register_browser_tools`]：注册为 `browser_*` 工具；`browser_cdp` 可发任意 CDP 命令，
//!   没有封装到的能力都能经它完成。

mod actions;
mod browser;
mod capture;
mod cdp;
mod input;
mod launch;
mod page;
mod session;
mod tools;

pub use actions::Snapshot;
pub use browser::{Browser, BrowserConfig, TabInfo, Target};
pub use cdp::{Cdp, CdpError, CdpEvent, CdpResult};
pub use tools::register_browser_tools;
