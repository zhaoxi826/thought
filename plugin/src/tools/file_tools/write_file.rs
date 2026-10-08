//! write_file 工具：为 agent 提供写文件的能力（覆盖写）。

use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use serde::Deserialize;
use tokio::fs;
use tracing::error;

/// 写文件工具：把内容写入指定路径，文件存在则覆盖。
pub struct WriteFile;

/// `write_file` 的入参，从模型回传的 JSON 参数串反序列化而来。
#[derive(Deserialize)]
struct WriteFileArgs {
    file_path: String,
    file_content: String,
}

#[async_trait::async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn definition(&self) -> Function {
        Function {
            name: "write_file".to_owned(),
            description: "A tool to write content to a file (overwrites if it exists)".to_owned(),
            parameters: vec![
                Parameter {
                    name: "file_path".to_owned(),
                    description: "Path to the file to write".to_owned(),
                    r#type: ParameterType::String,
                    required: true,
                },
                Parameter {
                    name: "file_content".to_owned(),
                    description: "Content to write into the file".to_owned(),
                    r#type: ParameterType::String,
                    required: true,
                },
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: WriteFileArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("write_file: invalid arguments {arguments:?}, err: {e}");
                return format!("invalid arguments for write_file: {e}");
            }
        };

        let len = args.file_content.len();
        match fs::write(&args.file_path, &args.file_content).await {
            Ok(()) => format!("wrote {len} bytes to {}", args.file_path),
            Err(e) => {
                error!("can't write file {}, err: {e}", args.file_path);
                format!("can't write file {}, err: {e}", args.file_path)
            }
        }
    }
}
