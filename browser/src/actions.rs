//! 页面操作：导航、快照、点击、输入、按键、滚动、截图、等待、cookie、上传。
//! 输入事件走 CDP `Input.*`（真实的鼠标 / 键盘事件，不是 JS 模拟），对框架友好。


pub(crate) const SNAPSHOT_JS: &str = include_str!("snapshot.js");

/// 页面快照。
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Snapshot {
    pub url: String,
    pub title: String,
    pub scroll: String,
    pub refs: u64,
    pub text: String,
}

impl Snapshot {
    pub fn render(&self) -> String {
        format!("{} — {}\n({}, {} interactive elements)\n\n{}", self.title, self.url, self.scroll, self.refs, self.text)
    }
}
