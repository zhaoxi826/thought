//! 内置工具集：文件读写删 + 命令执行。

pub mod delete_file;
pub mod read_file;
pub mod run_command;
pub mod write_file;

pub use delete_file::DeleteFile;
pub use read_file::ReadFile;
pub use run_command::RunCommand;
pub use write_file::WriteFile;

use crate::tool::Registry;
use std::sync::Arc;

/// 注册全部内置工具的 Registry。
pub fn builtin_registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(ReadFile))
        .register(Arc::new(WriteFile))
        .register(Arc::new(DeleteFile))
        .register(Arc::new(RunCommand::default()));
    registry
}
