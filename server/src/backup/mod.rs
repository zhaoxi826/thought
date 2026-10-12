//! 备份：把整个数据库（所有 namespace / database）导出、加密、上传到 S3 兼容对象存储。
//!
//! - 每天自动一次 + 随时手动；保留最近 `keep` 份，更早的删除；
//! - 加密在本机完成（口令派生密钥），对象存储只看到密文；
//! - 恢复不在网页里做：`thought restore <备份名>`（需先停服务，见 deploy/README.md）。

pub mod crypt;
pub mod s3;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use surrealdb::engine::any::Any;
use surrealdb::Surreal;
use tokio::sync::Mutex;

/// `[backup]` 配置。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BackupConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub s3: s3::S3Config,
    /// 加密口令（≥12 字符）。与 `passphrase_env` 二选一；丢了备份就打不开。
    #[serde(default, skip_serializing)]
    pub passphrase: Option<String>,
    #[serde(default)]
    pub passphrase_env: Option<String>,
    /// 每天几点（本地时区，0–23）自动备份。
    #[serde(default = "default_hour")]
    pub daily_hour: u8,
    /// 时区偏移（分钟），默认 UTC+8。
    #[serde(default = "default_tz")]
    pub tz_offset_min: i32,
    /// 保留最近几份。
    #[serde(default = "default_keep")]
    pub keep: usize,
}

fn default_true() -> bool {
    true
}
fn default_hour() -> u8 {
    4
}
fn default_tz() -> i32 {
    480
}
fn default_keep() -> usize {
    14
}

/// 备份文件名前缀与后缀：`thought-YYYYMMDD-HHMMSS.thbk`。
const NAME_PREFIX: &str = "thought-";
const NAME_SUFFIX: &str = ".thbk";

#[derive(Serialize, Debug, Clone, Default, PartialEq)]
pub struct BackupStatus {
    pub enabled: bool,
    pub running: bool,
    pub last_ok: Option<BackupRun>,
    pub last_error: Option<BackupRun>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BackupRun {
    pub at_ms: i64,
    pub name: Option<String>,
    pub bytes: Option<u64>,
    pub error: Option<String>,
    /// `daily` / `manual`
    pub trigger: String,
}

/// 备份器。`Clone` 廉价。
#[derive(Clone)]
pub struct Backup {
    db: Surreal<Any>,
    state_db: Surreal<Any>,
    config: Arc<std::sync::RwLock<Option<BackupConfig>>>,
    status: Arc<std::sync::Mutex<BackupStatus>>,
    /// 同一时刻只跑一个备份。
    running: Arc<Mutex<()>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn passphrase(cfg: &BackupConfig) -> Result<String, String> {
    cfg.passphrase
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| cfg.passphrase_env.as_deref().and_then(|v| std::env::var(v).ok()))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "backup: passphrase / passphrase_env is not set".to_owned())
}

/// 备份内容：每个 namespace / database 的导出 SQL。
#[derive(Serialize, Deserialize)]
pub struct Archive {
    pub version: u32,
    pub created_ms: i64,
    pub databases: Vec<(String, String, String)>,
}

impl Backup {
    /// `conn` 是服务共用的数据库连接；`ns`/`db` 是后端自身的存储位置（存备份状态）。
    pub async fn new(conn: &Surreal<Any>, ns: &str, db: &str, config: Option<BackupConfig>) -> Result<Self, surrealdb::Error> {
        let state_db = conn.clone();
        state_db.use_ns(ns).use_db(db).await?;
        state_db.query("DEFINE TABLE IF NOT EXISTS backup_run SCHEMALESS;").await?.check()?;
        let backup = Self {
            db: conn.clone(),
            state_db,
            config: Arc::new(std::sync::RwLock::new(config)),
            status: Arc::default(),
            running: Arc::default(),
        };
        backup.load_status().await;
        Ok(backup)
    }

    async fn load_status(&self) {
        let q = "SELECT * OMIT id FROM backup_run WHERE error = NONE ORDER BY at_ms DESC LIMIT 1;
                 SELECT * OMIT id FROM backup_run WHERE error != NONE ORDER BY at_ms DESC LIMIT 1;";
        let Ok(mut r) = self.state_db.query(q).await else { return };
        let mut take = |i: usize| -> Option<BackupRun> {
            let v: surrealdb::types::Value = r.take(i).ok()?;
            serde_json::from_value::<Vec<BackupRun>>(v.into_json_value()).ok()?.into_iter().next()
        };
        let (ok, err) = (take(0), take(1));
        let mut st = self.status.lock().expect("backup status poisoned");
        st.last_ok = ok;
        st.last_error = err.filter(|e| st.last_ok.as_ref().is_none_or(|o| e.at_ms > o.at_ms));
        st.enabled = self.config().is_some_and(|c| c.enabled);
    }

    pub fn config(&self) -> Option<BackupConfig> {
        self.config.read().expect("backup config poisoned").clone()
    }

    pub fn set_config(&self, config: Option<BackupConfig>) {
        self.status.lock().expect("backup status poisoned").enabled = config.as_ref().is_some_and(|c| c.enabled);
        *self.config.write().expect("backup config poisoned") = config;
    }

    pub fn status(&self) -> BackupStatus {
        let mut s = self.status.lock().expect("backup status poisoned").clone();
        s.running = self.running.try_lock().is_err();
        s
    }

    fn client(&self) -> Result<(s3::S3, String, usize), String> {
        let cfg = self.config().ok_or("backup is not configured")?;
        if !cfg.enabled {
            return Err("backup is disabled".into());
        }
        Ok((s3::S3::new(cfg.s3.clone())?, passphrase(&cfg)?, cfg.keep.max(1)))
    }

    /// 导出所有 namespace / database。
    async fn export_all(&self) -> Result<Archive, String> {
        let db = self.db.clone();
        let mut r = db.query("INFO FOR ROOT").await.map_err(|e| e.to_string())?;
        let info: surrealdb::types::Value = r.take(0).map_err(|e| e.to_string())?;
        let namespaces: Vec<String> = info.into_json_value()["namespaces"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
        let mut databases = Vec::new();
        for ns in namespaces {
            db.use_ns(&ns).await.map_err(|e| e.to_string())?;
            let mut r = db.query("INFO FOR NS").await.map_err(|e| e.to_string())?;
            let info: surrealdb::types::Value = r.take(0).map_err(|e| e.to_string())?;
            let dbs: Vec<String> = info.into_json_value()["databases"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
            for name in dbs {
                db.use_ns(&ns).use_db(&name).await.map_err(|e| e.to_string())?;
                let mut stream = db.export(()).await.map_err(|e| format!("export {ns}/{name}: {e}"))?;
                let mut sql = Vec::new();
                while let Some(chunk) = stream.next().await {
                    sql.extend(chunk.map_err(|e| format!("export {ns}/{name}: {e}"))?);
                }
                databases.push((ns.clone(), name, String::from_utf8_lossy(&sql).into_owned()));
            }
        }
        Ok(Archive { version: 1, created_ms: now_ms(), databases })
    }

    /// 立即备份一次。`trigger`：`daily` / `manual`。返回备份名。
    pub async fn run(&self, trigger: &str) -> Result<String, String> {
        let _guard = self.running.try_lock().map_err(|_| "a backup is already running".to_owned())?;
        let started = now_ms();
        let result = self.run_inner(started).await;
        let run = match &result {
            Ok((name, bytes)) => BackupRun { at_ms: started, name: Some(name.clone()), bytes: Some(*bytes), error: None, trigger: trigger.into() },
            Err(e) => BackupRun { at_ms: started, name: None, bytes: None, error: Some(e.clone()), trigger: trigger.into() },
        };
        let _ = self.state_db.query("CREATE backup_run CONTENT $r").bind(json!({ "r": run })).await;
        {
            let mut st = self.status.lock().expect("backup status poisoned");
            if run.error.is_none() {
                st.last_ok = Some(run.clone());
                st.last_error = None;
            } else {
                st.last_error = Some(run.clone());
            }
        }
        match &result {
            Ok((name, bytes)) => tracing::info!(name, bytes, trigger, "backup uploaded"),
            Err(e) => tracing::warn!(trigger, "backup failed: {e}"),
        }
        result.map(|(n, _)| n)
    }

    async fn run_inner(&self, started: i64) -> Result<(String, u64), String> {
        let (client, pass, keep) = self.client()?;
        let archive = self.export_all().await?;
        let plain = serde_json::to_vec(&archive).map_err(|e| e.to_string())?;
        let sealed = tokio::task::spawn_blocking(move || crypt::encrypt(&pass, &plain)).await.map_err(|e| e.to_string())??;
        let name = backup_name(started);
        let bytes = sealed.len() as u64;
        client.put(&name, sealed).await?;
        // 只保留最近 keep 份（按名字排序 = 按时间）
        let mut ours: Vec<String> = client.list().await?.into_iter().map(|o| o.key).filter(|k| is_backup_name(k)).collect();
        ours.sort();
        if ours.len() > keep {
            for old in &ours[..ours.len() - keep] {
                if let Err(e) = client.delete(old).await {
                    tracing::warn!("delete old backup {old}: {e}");
                }
            }
        }
        Ok((name, bytes))
    }

    /// 对象存储里的备份（新的在前）。
    pub async fn list(&self) -> Result<Vec<s3::Object>, String> {
        let (client, _, _) = self.client()?;
        let mut items: Vec<s3::Object> = client.list().await?.into_iter().filter(|o| is_backup_name(&o.key)).collect();
        items.sort_by(|a, b| b.key.cmp(&a.key));
        Ok(items)
    }

    /// 每天 `daily_hour` 点自动备份。每 5 分钟看一次；当天已成功备份过就跳过。
    pub fn spawn_daily(&self) -> tokio::task::JoinHandle<()> {
        let b = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                tick.tick().await;
                let Some(cfg) = b.config().filter(|c| c.enabled) else { continue };
                let now = now_ms();
                let local = now + i64::from(cfg.tz_offset_min) * 60_000;
                let hour = local.rem_euclid(86_400_000) / 3_600_000;
                let today = local.div_euclid(86_400_000);
                let done_today = b.status().last_ok.is_some_and(|r| (r.at_ms + i64::from(cfg.tz_offset_min) * 60_000).div_euclid(86_400_000) == today);
                // 失败后一小时内不重复尝试
                let failed_recently = b.status().last_error.is_some_and(|r| now - r.at_ms < 3_600_000);
                if hour >= i64::from(cfg.daily_hour) && !done_today && !failed_recently {
                    let _ = b.run("daily").await;
                }
            }
        })
    }
}

fn backup_name(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{NAME_PREFIX}{y:04}{m:02}{d:02}-{:02}{:02}{:02}{NAME_SUFFIX}", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn is_backup_name(key: &str) -> bool {
    key.starts_with(NAME_PREFIX) && key.ends_with(NAME_SUFFIX) && !key.contains('/')
}

/// 恢复（命令行用，服务须已停止）：下载、解密、写入到 `endpoint` 指向的存储。
/// 目标库会被清空后再导入。
pub async fn restore(endpoint: &str, cfg: &BackupConfig, name: &str) -> Result<usize, String> {
    if !is_backup_name(name) {
        return Err(format!("{name:?} is not a backup name (thought-YYYYMMDD-HHMMSS.thbk)"));
    }
    let client = s3::S3::new(cfg.s3.clone())?;
    let pass = passphrase(cfg)?;
    let sealed = client.get(name).await?;
    let plain = tokio::task::spawn_blocking(move || crypt::decrypt(&pass, &sealed)).await.map_err(|e| e.to_string())??;
    let archive: Archive = serde_json::from_slice(&plain).map_err(|e| format!("bad backup content: {e}"))?;
    let db = surrealdb::engine::any::connect(endpoint).await.map_err(|e| format!("open {endpoint}: {e}"))?;
    for (ns, name, sql) in &archive.databases {
        db.query(format!("DEFINE NAMESPACE IF NOT EXISTS `{ns}`")).await.map_err(|e| e.to_string())?;
        db.use_ns(ns).await.map_err(|e| e.to_string())?;
        db.query(format!("REMOVE DATABASE IF EXISTS `{name}`; DEFINE DATABASE `{name}`;")).await.map_err(|e| e.to_string())?.check().map_err(|e| e.to_string())?;
        db.use_ns(ns).use_db(name).await.map_err(|e| e.to_string())?;
        db.query(sql.as_str()).await.map_err(|e| format!("import {ns}/{name}: {e}"))?.check().map_err(|e| format!("import {ns}/{name}: {e}"))?;
    }
    // 关闭连接并等后台引擎释放文件锁，之后服务才能打开这个库
    drop(db);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    Ok(archive.databases.len())
}

/// 给 `/api/backup` 用的配置视图（不含密钥）。
pub fn public_config(cfg: &Option<BackupConfig>) -> Value {
    serde_json::to_value(cfg).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sort_by_time() {
        let a = backup_name(1_791_676_800_000);
        let b = backup_name(1_791_676_800_000 + 3_600_000);
        assert_eq!(a, "thought-20261011-000000.thbk");
        assert!(a < b && is_backup_name(&a));
        assert!(!is_backup_name("other.txt") && !is_backup_name("x/thought-1.thbk"));
    }

    #[tokio::test]
    async fn export_and_restore_roundtrip_through_archive() {
        let conn = surrealdb::engine::any::connect("mem://").await.unwrap();
        conn.use_ns("thought").use_db("server").await.unwrap();
        conn.query("CREATE stream:1 SET text = '我叫思想'").await.unwrap();
        conn.use_ns("thought").use_db("mind").await.unwrap();
        conn.query("CREATE paragraph:a SET content = '星星'").await.unwrap();
        let b = Backup::new(&conn, "thought", "server", None).await.unwrap();
        let archive = b.export_all().await.unwrap();
        let names: Vec<_> = archive.databases.iter().map(|(n, d, _)| format!("{n}/{d}")).collect();
        assert!(names.contains(&"thought/server".to_string()) && names.contains(&"thought/mind".to_string()), "{names:?}");

        // 加密往返 + 导入到全新库
        let sealed = crypt::encrypt("a long passphrase", &serde_json::to_vec(&archive).unwrap()).unwrap();
        let back: Archive = serde_json::from_slice(&crypt::decrypt("a long passphrase", &sealed).unwrap()).unwrap();
        let fresh = surrealdb::engine::any::connect("mem://").await.unwrap();
        for (ns, name, sql) in &back.databases {
            fresh.query(format!("DEFINE NAMESPACE IF NOT EXISTS `{ns}`")).await.unwrap();
            fresh.use_ns(ns).await.unwrap();
            fresh.query(format!("DEFINE DATABASE IF NOT EXISTS `{name}`")).await.unwrap();
            fresh.use_ns(ns).use_db(name).await.unwrap();
            fresh.query(sql.as_str()).await.unwrap().check().unwrap();
        }
        fresh.use_ns("thought").use_db("server").await.unwrap();
        let mut r = fresh.query("SELECT VALUE text FROM stream").await.unwrap();
        let v: surrealdb::types::Value = r.take(0).unwrap();
        assert_eq!(v.into_json_value(), json!(["我叫思想"]));
    }
}
