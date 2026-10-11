//! read_file：读取文件内容。相对路径相对工作区。

use super::parse_args;
use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use sandbox::Sandbox;
use serde::Deserialize;
use std::sync::Arc;

pub struct ReadFile(pub Arc<Sandbox>);

#[derive(Deserialize)]
struct Args {
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
            description: "Read a text file. Relative paths are inside your workspace.".to_owned(),
            parameters: vec![Parameter::required("file_path", "Path to the file", ParameterType::String)],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: Args = parse_args!("read_file", arguments);
        match self.0.fs.read(&args.file_path, self.0.max_read_bytes).await {
            Ok(content) => content,
            Err(e) => format!("can't read file: {e}"),
        }
    }
}
