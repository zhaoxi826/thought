//! 内置工具的离线测试：经沙箱的读写删 / 恢复 / 命令执行。

use plugin::tools::file_tools::builtin_registry;
use plugin::Registry;
use sandbox::{Isolation, Sandbox, SandboxConfig};
use std::path::PathBuf;
use std::sync::Arc;

struct Env {
    root: PathBuf,
    tools: Registry,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn env(tag: &str) -> Env {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("plugin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let config = SandboxConfig {
        workspace: root.join("ws"),
        trash: root.join("trash"),
        backup: root.join("backup"),
        command_timeout_secs: 5,
        ..Default::default()
    };
    let sandbox = Sandbox::with_isolation(&config, Isolation::None).unwrap();
    Env { root, tools: builtin_registry(Arc::new(sandbox)) }
}

async fn call(e: &Env, tool: &str, args: serde_json::Value) -> String {
    e.tools.call(tool, &args.to_string()).await.expect("tool registered")
}

#[tokio::test]
async fn write_read_delete_restore_roundtrip() {
    let e = env("rt");
    let w = call(&e, "write_file", serde_json::json!({"file_path": "notes/a.txt", "file_content": "hello thought"})).await;
    assert!(w.starts_with("wrote 13 bytes"), "{w}");
    assert_eq!(call(&e, "read_file", serde_json::json!({"file_path": "notes/a.txt"})).await, "hello thought");

    let d = call(&e, "delete_file", serde_json::json!({"file_path": "notes/a.txt"})).await;
    assert!(d.contains("moved") && d.contains("trash id: "), "{d}");
    let id = d.rsplit("trash id: ").next().unwrap().trim_end_matches(')');
    let gone = call(&e, "read_file", serde_json::json!({"file_path": "notes/a.txt"})).await;
    assert!(gone.starts_with("can't read file"), "{gone}");

    let list = call(&e, "restore_file", serde_json::json!({})).await;
    assert!(list.contains(id), "{list}");
    let r = call(&e, "restore_file", serde_json::json!({"trash_id": id})).await;
    assert!(r.starts_with("restored"), "{r}");
    assert_eq!(call(&e, "read_file", serde_json::json!({"file_path": "notes/a.txt"})).await, "hello thought");
}

#[tokio::test]
async fn external_write_reports_backup() {
    let e = env("ext");
    let outside = e.root.join("outside.txt");
    std::fs::write(&outside, "v1").unwrap();
    let w = call(&e, "write_file", serde_json::json!({"file_path": outside, "file_content": "v2"})).await;
    assert!(w.contains("backed up to"), "{w}");
}

#[tokio::test]
async fn run_command_reports_exit_code_and_output() {
    let e = env("cmd");
    let ok = call(&e, "run_command", serde_json::json!({"command": "echo hello"})).await;
    assert!(ok.contains("exit code: 0") && ok.contains("hello"), "{ok}");
    let fail = call(&e, "run_command", serde_json::json!({"command": "exit 3"})).await;
    assert!(fail.contains("exit code: 3"), "{fail}");
}

#[tokio::test]
async fn invalid_arguments_are_reported_as_text() {
    let e = env("bad");
    let r = e.tools.call("run_command", "not json").await.unwrap();
    assert!(r.contains("invalid arguments"), "{r}");
}

#[tokio::test]
async fn builtin_registry_registers_all_tools() {
    let e = env("reg");
    let names: Vec<&str> = e.tools.names().collect();
    assert_eq!(names, ["delete_file", "read_file", "restore_file", "run_command", "write_file"]);
}
