//! run_command 工具：为 agent 提供执行 shell 命令的能力。

use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use serde::Deserialize;
use tokio::process::Command;
use tracing::error;

/// 命令执行工具：通过 `sh -c` 运行命令，返回退出码与 stdout/stderr。
///
/// 超时后子进程被杀掉（`kill_on_drop`），避免挂住的命令永久占用 agent。
/// 注意：没有沙箱，命令以宿主进程的权限运行。
pub struct RunCommand {
    pub timeout: std::time::Duration,
}

impl Default for RunCommand {
    fn default() -> Self {
        Self {
            timeout: std::time::Duration::from_secs(120),
        }
    }
}

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

        let child = Command::new("sh")
            .arg("-c")
            .arg(&args.command)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        let result = match tokio::time::timeout(self.timeout, child).await {
            Ok(result) => result,
            Err(_) => {
                error!("command {:?} timed out", args.command);
                return format!("command timed out after {}s", self.timeout.as_secs());
            }
        };
        match result {
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
