//! run_command 工具：为 agent 提供执行 shell 命令的能力。

use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use serde::Deserialize;
use tokio::process::Command;
use tracing::error;

/// 命令执行工具：通过 `sh -c` 运行命令，返回退出码与 stdout/stderr。
pub struct RunCommand;

/// `run_command` 的入参，从模型回传的 JSON 参数串反序列化而来。
#[derive(Deserialize)]
struct RunCommandArgs {
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
            description: "A tool to run a shell command and return its output".to_owned(),
            parameters: vec![Parameter {
                name: "command".to_owned(),
                description: "The shell command to run".to_owned(),
                r#type: ParameterType::String,
                required: true,
            }],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: RunCommandArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("run_command: invalid arguments {arguments:?}, err: {e}");
                return format!("invalid arguments for run_command: {e}");
            }
        };

        match Command::new("sh").arg("-c").arg(&args.command).output().await {
            Ok(output) => {
                // code() 为 None 表示被信号终止
                let status = match output.status.code() {
                    Some(code) => code.to_string(),
                    None => format!("terminated by signal ({})", output.status),
                };
                format!(
                    "exit code: {status}\nstdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            }
            Err(e) => {
                error!("can't run command {:?}, err: {e}", args.command);
                format!("can't run command {:?}, err: {e}", args.command)
            }
        }
    }
}
