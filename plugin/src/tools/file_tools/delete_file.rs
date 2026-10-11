//! delete_file：删除文件——实际是移入回收站，可用 restore_file 恢复。

use super::parse_args;
use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use sandbox::Sandbox;
use serde::Deserialize;
use std::sync::Arc;

pub struct DeleteFile(pub Arc<Sandbox>);

#[derive(Deserialize)]
struct Args {
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
            description: "Delete a file. It is moved to the trash and can be restored with restore_file.".to_owned(),
            parameters: vec![Parameter::required("file_path", "Path to the file", ParameterType::String)],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: Args = parse_args!("delete_file", arguments);
        match self.0.fs.delete(&args.file_path).await {
            Ok(entry) => format!("moved {} to trash (trash id: {})", entry.original, entry.id),
            Err(e) => format!("can't delete file: {e}"),
        }
    }
}
