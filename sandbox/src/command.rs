//! 命令执行：优先用 bubblewrap 隔离，工作区可写、宿主只读、受保护路径不可见。
//!
//! 没有 bwrap 时退化为在工作区内直接执行（软边界），并在启动时记警告。

use crate::policy::FsPolicy;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// 一次命令执行的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// 退出码；被信号终止时为 `None`。
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// 是否因超时被杀。
    pub timed_out: bool,
    /// stdout / stderr 是否被截断。
    pub truncated: bool,
}

impl CommandOutput {
    /// 渲染成交给模型的文本。
    pub fn render(&self) -> String {
        let status = match (self.timed_out, self.code) {
            (true, _) => "timed out (killed)".to_owned(),
            (false, Some(code)) => code.to_string(),
            (false, None) => "terminated by signal".to_owned(),
        };
        let mut text = format!("exit code: {status}\nstdout:\n{}\nstderr:\n{}", self.stdout, self.stderr);
        if self.truncated {
            text.push_str("\n[output truncated]");
        }
        text
    }
}

/// 隔离方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Isolation {
    /// bubblewrap（路径）。
    Bwrap(PathBuf),
    /// 无隔离，只把工作目录设为工作区。
    None,
}

impl Isolation {
    /// 在 PATH 里找 bwrap 并试跑一次，确认非特权 user namespace 可用。
    pub fn detect() -> Self {
        let Some(path) = find_in_path("bwrap") else {
            return Isolation::None;
        };
        let ok = std::process::Command::new(&path)
            .args(["--ro-bind", "/", "/", "--unshare-all", "--die-with-parent", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok { Isolation::Bwrap(path) } else { Isolation::None }
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| Path::new(dir).join(name))
        .find(|p| p.is_file())
}

/// 构造 bwrap 参数：宿主根只读；工作区可写；受保护路径用空 tmpfs 遮住；
/// 新的 /tmp、/dev、/proc；可选断网。
fn bwrap_args(policy: &FsPolicy, network: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--ro-bind".into(), "/".into(), "/".into(),
        "--dev".into(), "/dev".into(),
        "--proc".into(), "/proc".into(),
        "--tmpfs".into(), "/tmp".into(),
    ];
    // 受保护路径：目录用 tmpfs 遮住；文件绑定为 /dev/null
    for p in policy.protected() {
        if p.is_dir() {
            args.extend(["--tmpfs".into(), p.to_string_lossy().into_owned()]);
        } else if p.exists() {
            args.extend(["--ro-bind".into(), "/dev/null".into(), p.to_string_lossy().into_owned()]);
        }
    }
    let ws = policy.workspace.to_string_lossy().into_owned();
    args.extend(["--bind".into(), ws.clone(), ws.clone(), "--chdir".into(), ws]);
    args.extend([
        "--unshare-user".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
    ]);
    if !network {
        args.push("--unshare-net".into());
    }
    args
}

/// 读至多 `limit` 字节，之后继续读完并丢弃（避免子进程因管道写满而阻塞）。
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut r: R, limit: usize) -> (Vec<u8>, bool) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        match r.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = limit.saturating_sub(buf.len());
                if room > 0 {
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                }
                if n > room {
                    truncated = true;
                }
            }
        }
    }
    (buf, truncated)
}

/// 命令执行器。
#[derive(Debug, Clone)]
pub struct CommandRunner {
    pub isolation: Isolation,
    pub network: bool,
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl CommandRunner {
    /// 用 `sh -c` 执行命令。
    pub async fn run(&self, policy: &FsPolicy, command: &str) -> std::io::Result<CommandOutput> {
        let mut cmd = match &self.isolation {
            Isolation::Bwrap(bwrap) => {
                let mut c = Command::new(bwrap);
                c.args(bwrap_args(policy, self.network)).args(["sh", "-c", command]);
                c
            }
            Isolation::None => {
                let mut c = Command::new("sh");
                c.arg("-c").arg(command).current_dir(&policy.workspace);
                c
            }
        };
        // 不继承服务进程的环境：那里有 JWT 密钥、模型 API key、管理员密码。
        // 只保留无害的基本变量；HOME 指向她的工作区。
        cmd.env_clear();
        for key in ["PATH", "LANG", "LC_ALL", "TZ", "TERM"] {
            if let Ok(v) = std::env::var(key) {
                cmd.env(key, v);
            }
        }
        if std::env::var_os("PATH").is_none() {
            cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
        }
        cmd.env("HOME", &policy.workspace);
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let stderr = child.stderr.take().expect("piped");
        let limit = self.max_output_bytes;
        let out_task = tokio::spawn(read_capped(stdout, limit));
        let err_task = tokio::spawn(read_capped(stderr, limit));

        let (code, timed_out) = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(status) => (status?.code(), false),
            Err(_) => {
                let _ = child.kill().await;
                (None, true)
            }
        };
        let (out, t1) = out_task.await.unwrap_or_default();
        let (err, t2) = err_task.await.unwrap_or_default();
        Ok(CommandOutput {
            code,
            stdout: String::from_utf8_lossy(&out).into_owned(),
            stderr: String::from_utf8_lossy(&err).into_owned(),
            timed_out,
            truncated: t1 || t2,
        })
    }
}
