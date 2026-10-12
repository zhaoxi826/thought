//! 备份端到端：本地模拟一个 S3（校验签名头存在、按路径式存取对象），
//! 走设置好的 [backup] 跑一次手动备份 → 列表 → 轮转 → 恢复到全新库。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type Store = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

/// 极简 S3：PUT / GET / DELETE /bucket/key，GET /bucket?list-type=2。要求带 SigV4 头。
async fn fake_s3() -> (String, Store) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let store: Store = Arc::default();
    let st = store.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let st = st.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 65536];
                let (head_end, len) = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let len = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0usize);
                        break (p + 4, len);
                    }
                };
                while buf.len() < head_end + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { break; }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let body = buf[head_end..].to_vec();
                let mut parts = head.split_whitespace();
                let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                let signed = head.to_lowercase().contains("authorization: aws4-hmac-sha256 credential=ak/");
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                let key = path.trim_start_matches("/bucket/").to_owned();
                let (status, out): (&str, Vec<u8>) = if !signed {
                    ("403 Forbidden", b"<Error><Code>AccessDenied</Code></Error>".to_vec())
                } else if method == "GET" && query.contains("list-type=2") {
                    let s = st.lock().unwrap();
                    let items: String = s.iter().map(|(k, v)| format!("<Contents><Key>{k}</Key><Size>{}</Size><LastModified>2026-10-12T00:00:00Z</LastModified></Contents>", v.len())).collect();
                    ("200 OK", format!("<ListBucketResult>{items}<IsTruncated>false</IsTruncated></ListBucketResult>").into_bytes())
                } else if method == "PUT" {
                    st.lock().unwrap().insert(key, body);
                    ("200 OK", Vec::new())
                } else if method == "GET" {
                    match st.lock().unwrap().get(&key) { Some(v) => ("200 OK", v.clone()), None => ("404 Not Found", b"<Error><Code>NoSuchKey</Code></Error>".to_vec()) }
                } else if method == "DELETE" {
                    st.lock().unwrap().remove(&key);
                    ("204 No Content", Vec::new())
                } else {
                    ("400 Bad Request", Vec::new())
                };
                let resp = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", out.len());
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&out).await;
            });
        }
    });
    (url, store)
}

#[tokio::test]
async fn backup_uploads_encrypted_rotates_and_restores() {
    let (s3_url, store) = fake_s3().await;
    let cfg: server::backup::BackupConfig = serde_json::from_value(serde_json::json!({
        "passphrase": "a very long passphrase",
        "keep": 2,
        "s3": { "endpoint": s3_url, "bucket": "bucket", "prefix": "", "access_key": "ak", "secret_key": "sk" }
    }))
    .unwrap();
    let conn = surrealdb::engine::any::connect("mem://").await.unwrap();
    conn.use_ns("thought").use_db("mind").await.unwrap();
    conn.query("CREATE stream:1 SET text = '我叫思想，今天是我出生的第一天'").await.unwrap();
    let b = server::backup::Backup::new(&conn, "thought", "server", Some(cfg.clone())).await.unwrap();

    // 三次备份（名字按秒，隔开一秒），只留两份
    let mut names = Vec::new();
    for _ in 0..3 {
        names.push(b.run("manual").await.unwrap());
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
    let listed: Vec<String> = b.list().await.unwrap().into_iter().map(|o| o.key).collect();
    assert_eq!(listed, vec![names[2].clone(), names[1].clone()], "keeps newest 2");
    // 对象存储里是密文
    let blob = store.lock().unwrap().get(&names[2]).cloned().unwrap();
    assert!(blob.starts_with(b"THBK1"));
    assert!(!String::from_utf8_lossy(&blob).contains("思想"));
    assert!(b.status().last_ok.is_some());

    // 恢复到一个全新的库
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("restore-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let endpoint = format!("surrealkv://{}", dir.display());
    let n = server::backup::restore(&endpoint, &cfg, &names[2]).await.unwrap();
    assert!(n >= 2, "server + mind databases");
    let mut fresh = None;
    for _ in 0..20 {
        match surrealdb::engine::any::connect(endpoint.as_str()).await {
            Ok(c) => { fresh = Some(c); break; }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(250)).await,
        }
    }
    let fresh = fresh.expect("restored db can be opened");
    fresh.use_ns("thought").use_db("mind").await.unwrap();
    let mut r = fresh.query("SELECT VALUE text FROM stream").await.unwrap();
    let v: surrealdb::types::Value = r.take(0).unwrap();
    assert_eq!(v.into_json_value(), serde_json::json!(["我叫思想，今天是我出生的第一天"]));
    drop(fresh);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // 口令错误、备份名非法都给出可读错误
    let mut wrong = cfg.clone();
    wrong.passphrase = Some("wrong wrong wrong".into());
    assert!(server::backup::restore(&endpoint, &wrong, &names[2]).await.unwrap_err().contains("wrong passphrase"));
    assert!(server::backup::restore(&endpoint, &cfg, "../etc/passwd").await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
