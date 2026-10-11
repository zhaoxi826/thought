//! 沙箱：她和外部世界之间的那层"可逆"的边界。
//!
//! - [`FsPolicy`]：工作区内自由；外部可读写，改前备份；删除一律进回收站；受保护路径不可触碰。
//! - [`CommandRunner`]：命令在 bubblewrap 里执行，宿主只读、工作区可写、受保护路径不可见，
//!   于是 shell 里改不了外部文件，外部文件只能经 [`FsPolicy`] 的可逆通道修改。
//!
//! [`Sandbox`] 把两者打包，供 plugin 的文件 / 命令工具共享。

mod command;
mod fs;
mod policy;

pub use command::{CommandOutput, CommandRunner, Isolation};
pub use fs::{FsError, TrashEntry};
pub use policy::{resolve, FsPolicy, SandboxConfig, Zone};

use std::time::Duration;

/// 共享的沙箱：策略 + 命令执行器 + 读上限。
#[derive(Debug, Clone)]
pub struct Sandbox {
    pub fs: FsPolicy,
    pub commands: CommandRunner,
    pub max_read_bytes: u64,
}

impl Sandbox {
    /// 按配置创建：建好目录、探测 bwrap。
    pub fn new(config: &SandboxConfig) -> std::io::Result<Self> {
        let isolation = Isolation::detect();
        if isolation == Isolation::None {
            tracing::warn!("bubblewrap not available; run_command falls back to an unsandboxed shell in the workspace");
        }
        Self::with_isolation(config, isolation)
    }

    pub fn with_isolation(config: &SandboxConfig, isolation: Isolation) -> std::io::Result<Self> {
        Ok(Self {
            fs: FsPolicy::new(config)?,
            commands: CommandRunner {
                isolation,
                network: config.network,
                timeout: Duration::from_secs(config.command_timeout_secs),
                max_output_bytes: config.max_output_bytes,
            },
            max_read_bytes: config.max_read_bytes,
        })
    }

    pub async fn run(&self, command: &str) -> std::io::Result<CommandOutput> {
        self.commands.run(&self.fs, command).await
    }
}
