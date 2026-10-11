//! write_file：写文件（覆盖）。工作区外的已有文件会先自动备份。

use super::parse_args;
use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use sandbox::Sandbox;
use serde::Deserialize;
use std::sync::Arc;

pub struct WriteFile(pub Arc<Sandbox>);

#[derive(Deserialize)]
struct Args {
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
            description: "Write content to a file, overwriting it. Relative paths are inside your \
                          workspace. Files outside the workspace are backed up before being changed."
                .to_owned(),
            parameters: vec![
                Parameter::required("file_path", "Path to the file", ParameterType::String),
                Parameter::required("file_content", "Full new content", ParameterType::String),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: Args = parse_args!("write_file", arguments);
        let len = args.file_content.len();
        match self.0.fs.write(&args.file_path, &args.file_content).await {
            Ok((abs, None)) => format!("wrote {len} bytes to {}", abs.display()),
            Ok((abs, Some(backup))) => {
                format!("wrote {len} bytes to {} (previous version backed up to {})", abs.display(), backup.display())
            }
            Err(e) => format!("can't write file: {e}"),
        }
    }
}
