//! 给 agent 用的记忆工具：`memory_search` / `memory_save` / `memory_forget`。

use crate::store::MemoryStore;
use crate::types::IngestRequest;
use model::{Function, Parameter, ParameterType};
use plugin::{Registry, Tool};
use serde::Deserialize;
use std::sync::Arc;

/// 注册三个记忆工具。`source` 标记经工具写入的记忆来源（如 `agent`）。
pub fn register_memory_tools(registry: &mut Registry, store: MemoryStore, source: &str) {
    registry
        .register(Arc::new(MemorySearch(store.clone())))
        .register(Arc::new(MemorySave {
            store: store.clone(),
            source: source.to_owned(),
        }))
        .register(Arc::new(MemoryForget(store)));
}

struct MemorySearch(MemoryStore);

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    5
}

#[async_trait::async_trait]
impl Tool for MemorySearch {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn definition(&self) -> Function {
        Function {
            name: "memory_search".to_owned(),
            description: "Search long-term memory for facts, preferences and past decisions.".to_owned(),
            parameters: vec![
                Parameter::required("query", "What to look for", ParameterType::String),
                Parameter::optional("limit", "Max results (default 5, max 20)", ParameterType::Integer),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: SearchArgs = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("invalid arguments for memory_search: {e}"),
        };
        match self.0.search(&args.query, args.limit.clamp(1, 20)).await {
            Ok(hits) if hits.is_empty() => "no relevant memories".to_owned(),
            Ok(hits) => hits
                .iter()
                .map(|h| {
                    let mut line = format!("[{}] (score {:.2}) {}", h.id, h.score, h.content);
                    if !h.relations.is_empty() {
                        line.push_str(&format!("\n  facts: {}", h.relations.join("; ")));
                    }
                    line
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("memory search failed: {e}"),
        }
    }
}

struct MemorySave {
    store: MemoryStore,
    source: String,
}

#[derive(Deserialize)]
struct SaveArgs {
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[async_trait::async_trait]
impl Tool for MemorySave {
    fn name(&self) -> &str {
        "memory_save"
    }

    fn definition(&self) -> Function {
        Function {
            name: "memory_save".to_owned(),
            description: "Save a durable fact, preference or decision to long-term memory. \
                          Write one self-contained statement."
                .to_owned(),
            parameters: vec![
                Parameter::required("content", "The statement to remember", ParameterType::String),
                Parameter::optional("tags", "Optional tags (array of strings)", ParameterType::Array),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: SaveArgs = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("invalid arguments for memory_save: {e}"),
        };
        let request = IngestRequest::new(args.content).source(&self.source).tags(args.tags);
        match self.store.ingest(request).await {
            Ok(r) if r.created => format!("saved as {}", r.id),
            Ok(r) => format!("already remembered as {}", r.id),
            Err(e) => format!("memory save failed: {e}"),
        }
    }
}

struct MemoryForget(MemoryStore);

#[derive(Deserialize)]
struct ForgetArgs {
    id: String,
}

#[async_trait::async_trait]
impl Tool for MemoryForget {
    fn name(&self) -> &str {
        "memory_forget"
    }

    fn definition(&self) -> Function {
        Function {
            name: "memory_forget".to_owned(),
            description: "Delete a memory by id (from memory_search results) when it is wrong or outdated."
                .to_owned(),
            parameters: vec![Parameter::required("id", "Memory id", ParameterType::String)],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: ForgetArgs = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("invalid arguments for memory_forget: {e}"),
        };
        match self.0.delete(&args.id).await {
            Ok(true) => format!("forgot {}", args.id),
            Ok(false) => format!("no memory with id {}", args.id),
            Err(e) => format!("memory forget failed: {e}"),
        }
    }
}
