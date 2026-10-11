//! 受策略约束的文件操作：读、写（外部先备份）、删（一律移入回收站）、恢复。

use crate::policy::{FsPolicy, Zone};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 文件操作错误：对外是一段人能读懂的话，会直接作为工具输出给模型。
#[derive(Debug)]
pub struct FsError(pub String);

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FsError {}

fn io(context: &str, path: &Path, e: std::io::Error) -> FsError {
    FsError(format!("{context} {}: {e}", path.display()))
}

/// 回收站里的一项。
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// 回收站内的相对路径（恢复时用它指定）。
    pub id: String,
    /// 原始绝对路径。
    pub original: String,
    pub deleted_at_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// UTC 日期 `YYYY-MM-DD`（不引入时间库，按天数推算）。
fn today() -> String {
    let days = now_ms().div_euclid(86_400_000);
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// 把绝对路径映射成存放目录下的相对路径（去掉根），保留原目录结构便于辨认。
fn mirror(root: &Path, original: &Path, stamp: i64) -> PathBuf {
    let rel: PathBuf = original.components().skip(1).collect();
    let mut target = root.join(today()).join(rel);
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.set_file_name(format!("{name}.{stamp}"));
    target
}

impl FsPolicy {
    fn check_touchable(&self, abs: &Path) -> Result<Zone, FsError> {
        match self.zone(abs) {
            Zone::Protected => Err(FsError(format!("{} is protected and cannot be accessed", abs.display()))),
            zone => Ok(zone),
        }
    }

    pub async fn read(&self, path: &str, max_bytes: u64) -> Result<String, FsError> {
        let abs = self.absolute(path);
        self.check_touchable(&abs)?;
        let meta = tokio::fs::metadata(&abs).await.map_err(|e| io("can't read", &abs, e))?;
        if meta.len() > max_bytes {
            return Err(FsError(format!(
                "{} is {} bytes, larger than the {max_bytes}-byte read limit",
                abs.display(),
                meta.len()
            )));
        }
        let bytes = tokio::fs::read(&abs).await.map_err(|e| io("can't read", &abs, e))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 写文件。工作区外的已有文件先备份；父目录不存在时自动创建。
    /// 返回 (绝对路径, 备份路径)。
    pub async fn write(&self, path: &str, content: &str) -> Result<(PathBuf, Option<PathBuf>), FsError> {
        let abs = self.absolute(path);
        let zone = self.check_touchable(&abs)?;
        let mut backup = None;
        if zone == Zone::External && tokio::fs::try_exists(&abs).await.unwrap_or(false) {
            let target = mirror(&self.backup, &abs, now_ms());
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| io("can't create backup dir", parent, e))?;
            }
            tokio::fs::copy(&abs, &target).await.map_err(|e| io("can't back up", &abs, e))?;
            backup = Some(target);
        }
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| io("can't create dir", parent, e))?;
        }
        tokio::fs::write(&abs, content).await.map_err(|e| io("can't write", &abs, e))?;
        Ok((abs, backup))
    }

    /// 删除：一律移入回收站（工作区内外都一样，可恢复）。返回回收站条目。
    pub async fn delete(&self, path: &str) -> Result<TrashEntry, FsError> {
        let abs = self.absolute(path);
        self.check_touchable(&abs)?;
        let meta = tokio::fs::symlink_metadata(&abs).await.map_err(|e| io("can't delete", &abs, e))?;
        if meta.is_dir() {
            return Err(FsError(format!("{} is a directory; only files can be deleted", abs.display())));
        }
        let stamp = now_ms();
        let target = mirror(&self.trash, &abs, stamp);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| io("can't create trash dir", parent, e))?;
        }
        // rename 跨文件系统会失败，退化为复制后删除
        if tokio::fs::rename(&abs, &target).await.is_err() {
            tokio::fs::copy(&abs, &target).await.map_err(|e| io("can't move to trash", &abs, e))?;
            tokio::fs::remove_file(&abs).await.map_err(|e| io("can't delete", &abs, e))?;
        }
        tokio::fs::write(meta_path(&target), abs.to_string_lossy().as_bytes())
            .await
            .map_err(|e| io("can't record trash origin", &target, e))?;
        Ok(TrashEntry {
            id: target.strip_prefix(&self.trash).unwrap_or(&target).to_string_lossy().into_owned(),
            original: abs.to_string_lossy().into_owned(),
            deleted_at_ms: stamp,
        })
    }

    /// 列出回收站。
    pub fn list_trash(&self) -> Vec<TrashEntry> {
        let mut out = Vec::new();
        let mut stack = vec![self.trash.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_some_and(|e| e == "origin") {
                    continue;
                }
                let original = std::fs::read_to_string(meta_path(&path)).unwrap_or_default();
                let stamp = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .and_then(|e| e.parse().ok())
                    .unwrap_or(0);
                out.push(TrashEntry {
                    id: path.strip_prefix(&self.trash).unwrap_or(&path).to_string_lossy().into_owned(),
                    original,
                    deleted_at_ms: stamp,
                });
            }
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.deleted_at_ms));
        out
    }

    /// 从回收站恢复到原位置；原位置已有文件时拒绝（不覆盖）。
    pub async fn restore(&self, id: &str) -> Result<PathBuf, FsError> {
        let item = crate::policy::resolve(&self.trash.join(id));
        if !item.starts_with(&self.trash) || item.extension().is_some_and(|e| e == "origin") {
            return Err(FsError(format!("no trash item {id:?}")));
        }
        let original = tokio::fs::read_to_string(meta_path(&item))
            .await
            .map_err(|_| FsError(format!("no trash item {id:?}")))?;
        let original = PathBuf::from(original);
        if tokio::fs::try_exists(&original).await.unwrap_or(false) {
            return Err(FsError(format!("{} already exists; not overwriting", original.display())));
        }
        if let Some(parent) = original.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| io("can't create dir", parent, e))?;
        }
        if tokio::fs::rename(&item, &original).await.is_err() {
            tokio::fs::copy(&item, &original).await.map_err(|e| io("can't restore", &item, e))?;
            let _ = tokio::fs::remove_file(&item).await;
        }
        let _ = tokio::fs::remove_file(meta_path(&item)).await;
        Ok(original)
    }

    /// 清理超过 `days` 天的回收站与备份目录（按日期目录名判断）。返回删除的日期目录数。
    pub fn purge_older_than(&self, days: i64) -> usize {
        let cutoff = now_ms() - days * 86_400_000;
        let mut removed = 0;
        for root in [&self.trash, &self.backup] {
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(ms) = date_to_ms(&name)
                    && ms < cutoff
                    && std::fs::remove_dir_all(entry.path()).is_ok()
                {
                    removed += 1;
                }
            }
        }
        removed
    }
}

fn meta_path(item: &Path) -> PathBuf {
    let mut p = item.as_os_str().to_owned();
    p.push(".origin");
    PathBuf::from(p)
}

/// `YYYY-MM-DD` → 当天 0 点的毫秒时间戳。
fn date_to_ms(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let (y, m, d): (i64, i64, i64) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // days_from_civil
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_roundtrip() {
        let t = today();
        let ms = date_to_ms(&t).unwrap();
        assert!(now_ms() - ms < 86_400_000 && now_ms() >= ms);
        assert_eq!(date_to_ms("2026-10-11"), Some(1_791_676_800_000));
        assert_eq!(date_to_ms("nope"), None);
    }
}
