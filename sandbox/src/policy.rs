//! 文件边界策略：工作区内自由；外部可读写但改前备份、删除改为移入回收站；
//! 受保护路径完全不可触碰。所有路径先规范化再判断，防 `..` 与软链绕过。

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// 沙箱配置（通常来自 `thought.toml` 的 `[sandbox]` 段）。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SandboxConfig {
    /// 她的工作区：完全自由，也是命令执行的工作目录。
    #[serde(default = "default_workspace")]
    pub workspace: PathBuf,

    /// 回收站：所有删除都移到这里，按日期分目录。
    #[serde(default = "default_trash")]
    pub trash: PathBuf,

    /// 外部文件修改前的备份，按日期分目录。
    #[serde(default = "default_backup")]
    pub backup: PathBuf,

    /// 不可读、不可写、不可删的路径（及其子路径）。数据库、配置、密钥等。
    #[serde(default)]
    pub protected: Vec<PathBuf>,

    /// 命令执行是否允许联网（bwrap 下生效）。
    #[serde(default = "default_true")]
    pub network: bool,

    /// 单条命令超时（秒）。
    #[serde(default = "default_timeout")]
    pub command_timeout_secs: u64,

    /// 单条命令 stdout / stderr 各自读取的上限（字节），超出截断。
    #[serde(default = "default_output")]
    pub max_output_bytes: usize,

    /// 读文件的上限（字节）。
    #[serde(default = "default_read")]
    pub max_read_bytes: u64,
}

fn default_workspace() -> PathBuf {
    PathBuf::from("data/mind/workspace")
}
fn default_trash() -> PathBuf {
    PathBuf::from("data/mind/trash")
}
fn default_backup() -> PathBuf {
    PathBuf::from("data/mind/backup")
}
fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    120
}
fn default_output() -> usize {
    256 * 1024
}
fn default_read() -> u64 {
    4 * 1024 * 1024
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            workspace: default_workspace(),
            trash: default_trash(),
            backup: default_backup(),
            protected: Vec::new(),
            network: true,
            command_timeout_secs: default_timeout(),
            max_output_bytes: default_output(),
            max_read_bytes: default_read(),
        }
    }
}

impl SandboxConfig {
    /// 相对路径改为相对 `base`（配置文件所在目录）。
    pub fn resolve_paths(&mut self, base: &Path) {
        for p in [&mut self.workspace, &mut self.trash, &mut self.backup] {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        for p in &mut self.protected {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
    }
}

/// 一个路径相对于策略的归属。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// 工作区内。
    Workspace,
    /// 工作区外的普通路径。
    External,
    /// 受保护（含回收站、备份目录本身）。
    Protected,
}

/// 规范化后的文件边界。
#[derive(Debug, Clone)]
pub struct FsPolicy {
    pub workspace: PathBuf,
    pub trash: PathBuf,
    pub backup: PathBuf,
    protected: Vec<PathBuf>,
}

/// 词法规范化：去掉 `.`，处理 `..`（不访问文件系统）。
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 尽可能解析软链：对存在的最长前缀 canonicalize，再拼上不存在的剩余部分。
/// 这样新文件的路径也能被正确判断，且软链指向工作区外时会被识别为外部。
pub fn resolve(path: &Path) -> PathBuf {
    let path = lexical_normalize(path);
    let mut existing = path.clone();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (existing.file_name().map(|n| n.to_owned()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return path,
        }
    }
}

impl FsPolicy {
    /// 创建策略并确保工作区 / 回收站 / 备份目录存在。
    pub fn new(config: &SandboxConfig) -> std::io::Result<Self> {
        for dir in [&config.workspace, &config.trash, &config.backup] {
            std::fs::create_dir_all(dir)?;
        }
        let mut protected: Vec<PathBuf> = config.protected.iter().map(|p| resolve(p)).collect();
        protected.push(resolve(&config.trash));
        protected.push(resolve(&config.backup));
        Ok(Self {
            workspace: resolve(&config.workspace),
            trash: resolve(&config.trash),
            backup: resolve(&config.backup),
            protected,
        })
    }

    /// 把调用方给的路径（相对路径相对工作区）解析成绝对路径。
    pub fn absolute(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        let joined = if p.is_absolute() { p.to_path_buf() } else { self.workspace.join(p) };
        resolve(&joined)
    }

    pub fn zone(&self, abs: &Path) -> Zone {
        // 受保护优先：即使在工作区内（例如有人把数据库软链进来），也不可触碰
        if self.protected.iter().any(|p| abs.starts_with(p)) {
            return Zone::Protected;
        }
        if abs.starts_with(&self.workspace) {
            Zone::Workspace
        } else {
            Zone::External
        }
    }

    pub fn protected(&self) -> &[PathBuf] {
        &self.protected
    }
}
