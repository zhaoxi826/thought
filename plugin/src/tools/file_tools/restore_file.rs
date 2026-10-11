//! restore_file：从回收站恢复文件到原位置（不覆盖已有文件）。

use super::parse_args;
use crate::tool::Tool;
use model::{Function, Parameter, ParameterType};
use sandbox::Sandbox;
use serde::Deserialize;
use std::sync::Arc;

pub struct RestoreFile(pub Arc<Sandbox>);

#[derive(Deserialize)]
struct Args {
    #[serde(default)]
    trash_id: Option<String>,
}

#[async_trait::async_trait]
impl Tool for RestoreFile {
    fn name(&self) -> &str {
        "restore_file"
    }

    fn definition(&self) -> Function {
        Function {
            name: "restore_file".to_owned(),
            description: "Restore a deleted file from the trash. Call without trash_id to list the trash.".to_owned(),
            parameters: vec![Parameter::optional("trash_id", "Id returned by delete_file", ParameterType::String)],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: Args = parse_args!("restore_file", arguments);
        let Some(id) = args.trash_id else {
            let items = self.0.fs.list_trash();
            if items.is_empty() {
                return "trash is empty".to_owned();
            }
            return items.iter().take(50).map(|t| format!("{}  ←  {}", t.id, t.original)).collect::<Vec<_>>().join("\n");
        };
        match self.0.fs.restore(&id).await {
            Ok(path) => format!("restored {}", path.display()),
            Err(e) => format!("can't restore: {e}"),
        }
    }
}
