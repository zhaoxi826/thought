//! delete_file 工具：为 agent 提供删除文件的能力。

use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use serde::Deserialize;
use tokio::fs;
use tracing::error;

/// 删除文件工具。
pub struct DeleteFile;

/// `delete_file` 的入参，从模型回传的 JSON 参数串反序列化而来。
#[derive(Deserialize)]
struct DeleteFileArgs {
    file_path: String,
}

#[async_trait::async_trait]
impl Tool for DeleteFile {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn definition(&self) -> Function {
        Function {
            name: "delete_file".to_owned(),
            description: "A tool to delete a file".to_owned(),
            parameters: vec![Parameter {
                name: "file_path".to_owned(),
                description: "Path to the file to delete".to_owned(),
                r#type: ParameterType::String,
                required: true,
            }],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: DeleteFileArgs = match serde_json::from_str(arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("delete_file: invalid arguments {arguments:?}, err: {e}");
                return format!("invalid arguments for delete_file: {e}");
            }
        };

        match fs::remove_file(&args.file_path).await {
            Ok(()) => format!("deleted file {}", args.file_path),
            Err(e) => {
                error!("can't delete file {}, err: {e}", args.file_path);
                format!("can't delete file {}, err: {e}", args.file_path)
            }
        }
    }
}
