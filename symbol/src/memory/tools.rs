//! 给 agent 用的记忆工具：`memory_search` / `memory_save` / `memory_forget` / `memory_share`。

use crate::memory::store::MemoryStore;
use crate::memory::types::{IngestRequest, Visibility};
use model::{Function, Parameter, ParameterType};
use plugin::{Registry, Tool};
use serde::Deserialize;
use std::sync::Arc;

/// 注册四个记忆工具。`source` 标记经工具写入的记忆来源（如 `agent`）。
pub fn register_memory_tools(registry: &mut Registry, store: MemoryStore, source: &str) {
    registry
        .register(Arc::new(MemorySearch(store.clone())))
        .register(Arc::new(MemorySave {
            store: store.clone(),
            source: source.to_owned(),
        }))
        .register(Arc::new(MemoryShare(store.clone())))
        .register(Arc::new(MemoryForget(store)));
}

fn visibility_label(v: Visibility) -> &'static str {
    match v {
        Visibility::Private => "private",
        Visibility::Shared => "shared",
        Visibility::Public => "public",
    }
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
                    let about = if h.about.is_empty() { String::new() } else { format!(" about {}", h.about.join(",")) };
                    let mut line = format!(
                        "[{}] (score {:.2}, {}{about}) {}",
                        h.id,
                        h.score,
                        visibility_label(h.visibility),
                        h.content
                    );
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
    #[serde(default)]
    about: Vec<String>,
    #[serde(default)]
    visibility: Visibility,
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
                Parameter::optional(
                    "about",
                    "Who this is about or who told you, e.g. [\"user:<id>\"] (array of strings)",
                    ParameterType::Array,
                ),
                Parameter::optional(
                    "visibility",
                    "private (default: someone's personal matter), shared (they said it can be told), public",
                    ParameterType::String,
                ),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: SaveArgs = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("invalid arguments for memory_save: {e}"),
        };
        let request = IngestRequest::new(args.content)
            .source(&self.source)
            .tags(args.tags)
            .about(args.about)
            .visibility(args.visibility);
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

struct MemoryShare(MemoryStore);

#[derive(Deserialize)]
struct ShareArgs {
    id: String,
    visibility: Visibility,
}

#[async_trait::async_trait]
impl Tool for MemoryShare {
    fn name(&self) -> &str {
        "memory_share"
    }

    fn definition(&self) -> Function {
        Function {
            name: "memory_share".to_owned(),
            description: "Change who a memory may be told to, e.g. after the person it is about \
                          says it is fine to share."
                .to_owned(),
            parameters: vec![
                Parameter::required("id", "Memory id", ParameterType::String),
                Parameter::required("visibility", "private, shared or public", ParameterType::String),
            ],
            strict: false,
        }
    }

    async fn call(&self, arguments: &str) -> String {
        let args: ShareArgs = match serde_json::from_str(arguments) {
            Ok(a) => a,
            Err(e) => return format!("invalid arguments for memory_share: {e}"),
        };
        match self.0.set_visibility(&args.id, args.visibility).await {
            Ok(true) => format!("{} is now {}", args.id, visibility_label(args.visibility)),
            Ok(false) => format!("no memory with id {}", args.id),
            Err(e) => format!("memory share failed: {e}"),
        }
    }
}
