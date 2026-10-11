//! 内置工具集：文件读写删、回收站恢复、命令执行。全部经 [`Sandbox`] 执行：
//! 工作区内自由；外部可读写但改前备份；删除一律进回收站；受保护路径不可触碰；
//! 命令在 bubblewrap 里执行，shell 改不了外部文件。

pub mod delete_file;
pub mod read_file;
pub mod restore_file;
pub mod run_command;
pub mod write_file;

pub use delete_file::DeleteFile;
pub use read_file::ReadFile;
pub use restore_file::RestoreFile;
pub use run_command::RunCommand;
pub use write_file::WriteFile;

use crate::tool::Registry;
use sandbox::Sandbox;
use std::sync::Arc;

/// 注册全部内置工具，共享同一个沙箱。
pub fn builtin_registry(sandbox: Arc<Sandbox>) -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(ReadFile(Arc::clone(&sandbox))))
        .register(Arc::new(WriteFile(Arc::clone(&sandbox))))
        .register(Arc::new(DeleteFile(Arc::clone(&sandbox))))
        .register(Arc::new(RestoreFile(Arc::clone(&sandbox))))
        .register(Arc::new(RunCommand(sandbox)));
    registry
}

/// 解析参数，失败时把错误文本作为工具输出返回给模型。
macro_rules! parse_args {
    ($tool:literal, $arguments:expr) => {
        match serde_json::from_str($arguments) {
            Ok(args) => args,
            Err(e) => {
                tracing::error!("{}: invalid arguments {:?}, err: {e}", $tool, $arguments);
                return format!("invalid arguments for {}: {e}", $tool);
            }
        }
    };
}
pub(crate) use parse_args;
