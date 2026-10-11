//! 沙箱的行为测试：可逆的文件边界 + bwrap 命令隔离。

use sandbox::{Isolation, Sandbox, SandboxConfig, Zone};
use std::path::PathBuf;

struct Env {
    root: PathBuf,
    sandbox: Sandbox,
    /// 工作区外的普通目录（模拟"外部世界"）。
    outside: PathBuf,
    /// 受保护目录（模拟数据库目录）。
    secret: PathBuf,
}

fn env(tag: &str, isolation: Option<Isolation>) -> Env {
    // 不放 /tmp：沙箱内 /tmp 是全新 tmpfs，会遮住宿主 /tmp，测不出"宿主只读"
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("sandbox-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let outside = root.join("outside");
    let secret = root.join("secret");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&secret).unwrap();
    std::fs::write(outside.join("notes.txt"), "original").unwrap();
    std::fs::write(secret.join("db"), "do not touch").unwrap();
    let config = SandboxConfig {
        workspace: root.join("ws"),
        trash: root.join("trash"),
        backup: root.join("backup"),
        protected: vec![secret.clone()],
        command_timeout_secs: 5,
        max_output_bytes: 1024,
        ..Default::default()
    };
    let sandbox = match isolation {
        Some(i) => Sandbox::with_isolation(&config, i).unwrap(),
        None => Sandbox::new(&config).unwrap(),
    };
    Env { root, sandbox, outside, secret }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn workspace_is_free_and_relative_paths_land_there() {
    let e = env("ws", Some(Isolation::None));
    let (abs, backup) = e.sandbox.fs.write("a/b.txt", "hi").await.unwrap();
    assert_eq!(abs, e.sandbox.fs.workspace.join("a/b.txt"));
    assert!(backup.is_none(), "workspace writes are not backed up");
    assert_eq!(e.sandbox.fs.read("a/b.txt", 1024).await.unwrap(), "hi");
    assert_eq!(e.sandbox.fs.zone(&abs), Zone::Workspace);
}

#[tokio::test]
async fn external_writes_are_backed_up_and_deletes_go_to_trash() {
    let e = env("ext", Some(Isolation::None));
    let notes = e.outside.join("notes.txt");
    let path = notes.to_str().unwrap();

    let (_, backup) = e.sandbox.fs.write(path, "changed").await.unwrap();
    let backup = backup.expect("external overwrite must back up");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), "original");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "changed");

    let entry = e.sandbox.fs.delete(path).await.unwrap();
    assert!(!notes.exists(), "file moved away");
    assert_eq!(entry.original, notes.to_string_lossy());
    let trash = e.sandbox.fs.list_trash();
    assert_eq!(trash.len(), 1);
    assert_eq!(trash[0].id, entry.id);

    let restored = e.sandbox.fs.restore(&entry.id).await.unwrap();
    assert_eq!(restored, notes);
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "changed");
    assert!(e.sandbox.fs.list_trash().is_empty());

    // 恢复不覆盖已存在文件
    let entry = e.sandbox.fs.delete(path).await.unwrap();
    std::fs::write(&notes, "new one").unwrap();
    assert!(e.sandbox.fs.restore(&entry.id).await.is_err());
    // 回收站路径穿越被拒绝
    assert!(e.sandbox.fs.restore("../outside/notes.txt").await.is_err());
}

#[tokio::test]
async fn protected_paths_are_untouchable_even_via_dotdot_or_symlink() {
    let e = env("prot", Some(Isolation::None));
    let db = e.secret.join("db");
    assert!(e.sandbox.fs.read(db.to_str().unwrap(), 1024).await.is_err());
    assert!(e.sandbox.fs.write(db.to_str().unwrap(), "x").await.is_err());
    assert!(e.sandbox.fs.delete(db.to_str().unwrap()).await.is_err());

    // 相对路径 .. 逃逸
    assert!(e.sandbox.fs.read("../secret/db", 1024).await.is_err());
    // 工作区里的软链指向受保护目录
    std::os::unix::fs::symlink(&e.secret, e.sandbox.fs.workspace.join("link")).unwrap();
    assert!(e.sandbox.fs.read("link/db", 1024).await.is_err());
    assert!(e.sandbox.fs.delete("link/db").await.is_err());
    // 回收站、备份目录本身也受保护
    let trash_file = e.sandbox.fs.trash.join("x");
    assert!(e.sandbox.fs.write(trash_file.to_str().unwrap(), "x").await.is_err());
    assert_eq!(std::fs::read_to_string(&db).unwrap(), "do not touch");
}

#[tokio::test]
async fn read_limit_and_directory_delete_rejected() {
    let e = env("lim", Some(Isolation::None));
    e.sandbox.fs.write("big.txt", &"x".repeat(2000)).await.unwrap();
    assert!(e.sandbox.fs.read("big.txt", 1000).await.unwrap_err().0.contains("read limit"));
    std::fs::create_dir_all(e.sandbox.fs.workspace.join("dir")).unwrap();
    assert!(e.sandbox.fs.delete("dir").await.is_err());
}

#[tokio::test]
async fn command_output_is_capped_and_timeouts_kill() {
    let e = env("cmd", Some(Isolation::None));
    let out = e.sandbox.run("yes | head -c 5000").await.unwrap();
    assert_eq!(out.stdout.len(), 1024);
    assert!(out.truncated);
    let out = e.sandbox.run("pwd").await.unwrap();
    assert_eq!(out.stdout.trim(), e.sandbox.fs.workspace.to_string_lossy());

    let mut slow = e.sandbox.clone();
    slow.commands.timeout = std::time::Duration::from_millis(200);
    let out = slow.run("sleep 5").await.unwrap();
    assert!(out.timed_out);
    assert!(out.render().contains("timed out"));
}

/// bwrap 下：shell 改不了外部文件，看不见受保护目录，但工作区可写。
#[tokio::test]
async fn bwrap_isolates_shell_from_outside_and_protected() {
    let isolation = Isolation::detect();
    if isolation == Isolation::None {
        eprintln!("bwrap unavailable; skipping");
        return;
    }
    let e = env("bwrap", Some(isolation));
    let notes = e.outside.join("notes.txt");
    let db = e.secret.join("db");

    let out = e.sandbox.run(&format!("echo hacked > {}; rm -f {}; echo done", notes.display(), notes.display())).await.unwrap();
    assert!(out.stdout.contains("done"), "{out:?}");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "original", "outside file must survive shell");

    let out = e.sandbox.run(&format!("cat {}", db.display())).await.unwrap();
    assert!(!out.stdout.contains("do not touch"), "protected must be invisible: {out:?}");

    let out = e.sandbox.run("echo ok > made.txt && cat made.txt && pwd").await.unwrap();
    assert!(out.stdout.contains("ok"), "{out:?}");
    assert!(e.sandbox.fs.workspace.join("made.txt").exists());

    // 外部仍然可读
    let out = e.sandbox.run(&format!("cat {}", notes.display())).await.unwrap();
    assert!(out.stdout.contains("original"));
}
