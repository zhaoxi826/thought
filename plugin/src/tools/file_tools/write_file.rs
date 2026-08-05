//! write_file工具： 为agent提供写文件的能力

use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use serde::Deserialize;
use tokio::fs;
use tracing::error;

pub struct WriteFile;

#[derive(Deserialize)]
struct ReadFileArgs {
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
            description: "A tool to write file contents".to_owned(),
            parameters: vec![
                Parameter {
                    name: "file_path".to_owned(),
                    description: "Path to the file to read".to_owned(),
                    r#type: ParameterType::String,
                    required: true,
                },
                Parameter {
                    name: "file_content".to
                }
            ],
            strict: false,
        }
    }
}