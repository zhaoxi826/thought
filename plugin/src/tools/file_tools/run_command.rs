//! run_command：在沙箱里执行 shell 命令（工作目录为工作区）。
//!
//! bubblewrap 下宿主文件系统只读、受保护路径不可见，所以命令改不了工作区外的文件；
//! 要改外部文件请用 write_file（自动备份），要删请用 delete_file（进回收站）。

use super::parse_args;
use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use sandbox::Sandbox;
use serde::Deserialize;
use std::sync::Arc;

pub struct RunCommand(pub Arc<Sandbox>);

#[derive(Deserialize)]
struct Args {
    command: String,
}

#[async_trait::async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn definition(&self) -> Function {
        Function {
            name: "run_command".to_owned(),
            description: "Run a shell command with your workspace as the working directory. Files \
                          outside the workspace are read-only here; change them with write_file or \
                          delete_file instead."
                .to_owned(),
            parameters: vec![Parameter::required("command", "The shell command to run", ParameterType::String)],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: Args = parse_args!("run_command", arguments);
        match self.0.run(&args.command).await {
            Ok(output) => output.render(),
            Err(e) => {
                tracing::error!("can't run command {:?}, err: {e}", args.command);
                format!("can't run command: {e}")
            }
        }
    }
}
