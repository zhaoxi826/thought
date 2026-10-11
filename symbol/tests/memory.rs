//! 记忆器离线测试：内存 SurrealDB + 确定性 embedder + 脚本化抽取器。

use std::collections::HashMap;
use std::sync::Arc;
use symbol::{
    text, Embedder, ExtractedRelation, Extraction, Extractor, IngestRequest, MemoryConfig,
    MemoryError, MemoryStore,
};

const DIM: usize = 16;

/// 词袋哈希 embedder：同词 → 同维度，足够让“共享词的文本更相近”。
struct BagEmbedder;

#[async_trait::async_trait]
impl Embedder for BagEmbedder {
    fn dimension(&self) -> usize {
        DIM
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        Ok(inputs
            .iter()
            .map(|input| {
                let mut v = vec![0.0f32; DIM];
                for term in text::search_terms(input).split(' ').filter(|t| !t.is_empty()) {
                    let h = text::content_hash(term);
                    let i = usize::from_str_radix(&h[..4], 16).unwrap() % DIM;
                    v[i] += 1.0;
                }
                if v.iter().all(|x| *x == 0.0) {
                    v[0] = 1.0;
                }
                v
            })
            .collect())
    }
}

type Triple = (&'static str, &'static str, &'static str);

/// 按段落内容查表返回预置三元组。
struct ScriptedExtractor(HashMap<String, Vec<Triple>>);

#[async_trait::async_trait]
impl Extractor for ScriptedExtractor {
    async fn extract(&self, text: &str) -> Result<Extraction, MemoryError> {
        let Some(triples) = self.0.get(text) else {
            return Err(MemoryError::Invalid("no script".into()));
        };
        Ok(Extraction {
            entities: vec![],
            relations: triples
                .iter()
                .map(|(s, p, o)| ExtractedRelation {
                    subject: s.to_string(),
                    predicate: p.to_string(),
                    object: o.to_string(),
                    confidence: 0.9,
                })
                .collect(),
        })
    }
}

fn config() -> MemoryConfig {
    MemoryConfig {
        endpoint: "mem://".into(),
        embedding_dimension: DIM,
        ..Default::default()
    }
}

async fn store_with(script: Vec<(&str, Vec<Triple>)>) -> MemoryStore {
    let script = script.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    MemoryStore::open(
        config(),
        Some(Arc::new(BagEmbedder)),
        Some(Arc::new(ScriptedExtractor(script))),
    )
    .await
    .unwrap()
}

/// 填充一些无关段落，让 BM25 的 IDF 有意义。
async fn filler(store: &MemoryStore) {
    for i in 0..12 {
        store
            .ingest(IngestRequest::new(format!("filler note number {i} about weather and lunch")))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn ingest_dedups_by_content_and_external_id() {
    let store = store_with(vec![]).await;
    let a = store.ingest(IngestRequest::new("  用户喜欢 Rust  ").tags(vec!["pref".into()])).await.unwrap();
    assert!(a.created);
    let b = store.ingest(IngestRequest::new("用户喜欢 Rust").tags(vec!["lang".into()])).await.unwrap();
    assert_eq!(b, symbol::IngestResult { id: a.id.clone(), created: false });
    let mem = store.get(&a.id).await.unwrap().unwrap();
    assert_eq!(mem.content, "用户喜欢 Rust");
    assert_eq!(mem.tags, vec!["pref".to_string(), "lang".to_string()]);

    let c = store.ingest(IngestRequest::new("first").external_id("msg-1")).await.unwrap();
    let d = store.ingest(IngestRequest::new("edited text").external_id("msg-1")).await.unwrap();
    assert_eq!(c.id, d.id);
    assert!(!d.created);

    assert!(matches!(store.ingest(IngestRequest::new("   ")).await, Err(MemoryError::Invalid(_))));
    assert_eq!(store.stats().await.unwrap().paragraphs, 2);
}

#[tokio::test]
async fn search_fuses_sparse_semantic_and_graph() {
    let store = store_with(vec![
        ("Alice 在北京的字节跳动工作", vec![("Alice", "works_at", "字节跳动"), ("字节跳动", "located_in", "北京")]),
        ("Bob 喜欢用 Rust 写后端", vec![("Bob", "likes", "Rust")]),
    ])
    .await;
    filler(&store).await;
    let alice = store.ingest(IngestRequest::new("Alice 在北京的字节跳动工作").source("chat:1")).await.unwrap();
    let bob = store.ingest(IngestRequest::new("Bob 喜欢用 Rust 写后端").source("chat:1")).await.unwrap();

    assert_eq!(store.stats().await.unwrap().pending_extraction, 14);
    // filler 没有脚本，抽取失败也会被标记为已处理
    assert_eq!(store.process_pending(100).await.unwrap(), 14);
    let stats = store.stats().await.unwrap();
    assert_eq!((stats.pending_extraction, stats.relations), (0, 3));

    let hits = store.search("北京 工作", 5).await.unwrap();
    assert_eq!(hits[0].id, alice.id, "{hits:#?}");
    assert!(hits[0].channels.sparse > 0.0);

    // 只命中关系词（works_at）也能经图通道找回段落
    let hits = store.search("who works_at 字节跳动", 5).await.unwrap();
    assert_eq!(hits[0].id, alice.id, "{hits:#?}");
    assert!(hits[0].channels.graph > 0.0);
    assert!(hits[0].relations.iter().any(|r| r == "Alice -works_at-> 字节跳动"));

    let hits = store.search("Rust", 5).await.unwrap();
    assert_eq!(hits[0].id, bob.id);

    let relations = store.relations_of("alice").await.unwrap();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].object, "字节跳动");
}

#[tokio::test]
async fn works_without_embedder() {
    let store = MemoryStore::open(config(), None, None).await.unwrap();
    filler(&store).await;
    let id = store.ingest(IngestRequest::new("项目代号叫 thought")).await.unwrap().id;
    let hits = store.search("thought 代号", 3).await.unwrap();
    assert_eq!(hits[0].id, id);
    assert_eq!(hits[0].channels.semantic, 0.0);
}

#[tokio::test]
async fn delete_detaches_and_prunes_orphan_relations() {
    let store = store_with(vec![
        ("Carol 住在上海", vec![("Carol", "lives_in", "上海")]),
        ("Carol 确实住在上海浦东", vec![("Carol", "lives_in", "上海")]),
    ])
    .await;
    let a = store.ingest(IngestRequest::new("Carol 住在上海").source("s1")).await.unwrap();
    let _b = store.ingest(IngestRequest::new("Carol 确实住在上海浦东").source("s2")).await.unwrap();
    store.process_pending(10).await.unwrap();

    let rel = store.relations_of("Carol").await.unwrap();
    assert_eq!(rel.len(), 1);
    assert_eq!(rel[0].paragraphs.len(), 2, "same triple merges supporting paragraphs");

    assert!(store.delete(&a.id).await.unwrap());
    assert!(!store.delete(&a.id).await.unwrap());
    assert_eq!(store.relations_of("Carol").await.unwrap()[0].paragraphs.len(), 1);

    assert_eq!(store.delete_by_source("s2").await.unwrap(), 1);
    let stats = store.stats().await.unwrap();
    assert_eq!((stats.paragraphs, stats.relations), (0, 0));
}

#[tokio::test]
async fn maintain_freezes_and_prunes_decayed_relations() {
    let mut cfg = config();
    // 冻结区间放宽到 (0, 0.1)：衰减后进入冻结而非删除
    cfg.prune_threshold = 0.0;
    // 半衰期极短：写入后很快衰减到阈值以下
    cfg.half_life_hours = 1e-7;
    let script: HashMap<_, _> = [("Dave 用 Vim".to_owned(), vec![("Dave", "uses", "Vim")])].into();
    let store = MemoryStore::open(cfg, Some(Arc::new(BagEmbedder)), Some(Arc::new(ScriptedExtractor(script))))
        .await
        .unwrap();
    store.ingest(IngestRequest::new("Dave 用 Vim")).await.unwrap();
    store.process_pending(10).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let rel_id = store.relations_of("Dave").await.unwrap()[0].id.clone();
    assert!(store.pin_relation(&rel_id, true).await.unwrap());
    assert_eq!(store.maintain().await.unwrap(), (0, 0), "pinned relations never decay");

    store.pin_relation(&rel_id, false).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(store.maintain().await.unwrap(), (1, 0));
    assert_eq!(store.stats().await.unwrap().inactive_relations, 1);
    // 冻结的关系不再参与图检索
    let hits = store.search("uses Vim", 5).await.unwrap();
    assert!(hits.iter().all(|h| h.relations.is_empty()), "{hits:#?}");
}

#[tokio::test]
async fn scoped_spaces_are_isolated() {
    let store = MemoryStore::open(config(), None, None).await.unwrap();
    let alice = store.scoped("user_alice").await.unwrap();
    let bob = store.scoped("user_bob").await.unwrap();
    alice.ingest(IngestRequest::new("secret of alice")).await.unwrap();
    assert_eq!(alice.stats().await.unwrap().paragraphs, 1);
    assert_eq!(bob.stats().await.unwrap().paragraphs, 0);
    assert!(store.scoped("bad name; DROP").await.is_err());
}

#[tokio::test]
async fn memory_tools_roundtrip() {
    let store = MemoryStore::open(config(), None, None).await.unwrap();
    filler(&store).await;
    let mut registry = plugin::Registry::new();
    symbol::register_memory_tools(&mut registry, store.clone(), "agent");

    let saved = registry
        .call("memory_save", r#"{"content":"部署环境是 Debian 13","tags":["infra"]}"#)
        .await
        .unwrap();
    assert!(saved.starts_with("saved as "), "{saved}");
    let id = saved.trim_start_matches("saved as ");

    let found = registry.call("memory_search", r#"{"query":"Debian 部署"}"#).await.unwrap();
    assert!(found.contains("部署环境是 Debian 13"), "{found}");

    let forgot = registry.call("memory_forget", &format!(r#"{{"id":"{id}"}}"#)).await.unwrap();
    assert_eq!(forgot, format!("forgot {id}"));
    assert_eq!(store.get(id).await.unwrap(), None);
}

#[tokio::test]
async fn surrealkv_persists_across_reopen() {
    let dir = std::env::temp_dir().join(format!("thought-memory-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cfg = MemoryConfig {
        endpoint: format!("surrealkv://{}", dir.display()),
        ..config()
    };
    let id = {
        let store = MemoryStore::open(cfg.clone(), None, None).await.unwrap();
        store.ingest(IngestRequest::new("persist me")).await.unwrap().id
    };
    // 等后台引擎释放文件锁
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let store = MemoryStore::open(cfg, None, None).await.unwrap();
    assert_eq!(store.get(&id).await.unwrap().unwrap().content, "persist me");
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn about_and_visibility_roundtrip_through_tools() {
    let store = MemoryStore::open(config(), None, None).await.unwrap();
    filler(&store).await;
    let mut registry = plugin::Registry::new();
    symbol::register_memory_tools(&mut registry, store.clone(), "mind");

    let saved = registry
        .call("memory_save", r#"{"content":"小王下个月要换工作","about":["user:wang"]}"#)
        .await
        .unwrap();
    let id = saved.trim_start_matches("saved as ").to_owned();
    let mem = store.get(&id).await.unwrap().unwrap();
    assert_eq!(mem.about, vec!["user:wang".to_string()]);
    assert_eq!(mem.visibility, symbol::Visibility::Private);

    let found = registry.call("memory_search", r#"{"query":"换工作"}"#).await.unwrap();
    assert!(found.contains("private about user:wang"), "{found}");

    let shared = registry
        .call("memory_share", &format!(r#"{{"id":"{id}","visibility":"shared"}}"#))
        .await
        .unwrap();
    assert_eq!(shared, format!("{id} is now shared"));
    assert_eq!(store.get(&id).await.unwrap().unwrap().visibility, symbol::Visibility::Shared);
    assert!(registry.call("memory_share", r#"{"id":"nope","visibility":"public"}"#).await.unwrap().starts_with("no memory"));
}
