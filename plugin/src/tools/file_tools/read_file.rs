//! read_file 工具：为 agent 提供读取文件内容的能力。

use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use serde::Deserialize;
use tokio::fs;
use tracing::error;

/// 读取文件工具。
pub struct ReadFile;

/// `read_file` 的入参，从模型回传的 JSON 参数串反序列化而来。
#[derive(Deserialize)]
struct ReadFileArgs {
    file_path: String,
}

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn definition(&self) -> Function {
        Function {
            name: "read_file".to_owned(),
            description: "A tool to read file contents".to_owned(),
            parameters: vec![Parameter {
                name: "file_path".to_owned(),
                description: "Path to the file to read".to_owned(),
                r#type: ParameterType::String,
                required: true,
            }],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: ReadFileArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("read_file: invalid arguments {arguments:?}, err: {e}");
                return format!("invalid arguments for read_file: {e}");
            }
        };

        fs::read_to_string(&args.file_path).await.unwrap_or_else(|e| {
            error!("can't read file {}, err: {e}", args.file_path);
            format!("can't read file {}, err: {e}", args.file_path)
        })
    }
}
