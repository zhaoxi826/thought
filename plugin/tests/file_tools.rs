//! 内置工具的离线测试：真实文件读写删（临时目录）与命令执行。

use plugin::tools::file_tools::{DeleteFile, ReadFile, RunCommand, WriteFile, builtin_registry};
use plugin::Tool;

fn temp_file(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("thought-plugin-test-{}-{name}", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn write_read_delete_roundtrip() {
    let path = temp_file("roundtrip");
    let args = |extra: &str| format!("{{\"file_path\": \"{path}\", {extra}}}");

    let write = WriteFile
        .call(&args("\"file_content\": \"hello thought\""))
        .await;
    assert!(write.contains("wrote"), "write result: {write}");

    let content = ReadFile
        .call(&format!("{{\"file_path\": \"{path}\"}}"))
        .await;
    assert_eq!(content, "hello thought");

    let delete = DeleteFile
        .call(&format!("{{\"file_path\": \"{path}\"}}"))
        .await;
    assert!(delete.contains("deleted"), "delete result: {delete}");

    let gone = ReadFile
        .call(&format!("{{\"file_path\": \"{path}\"}}"))
        .await;
    assert!(gone.contains("can't read file"), "read after delete: {gone}");
}

#[tokio::test]
async fn write_file_reports_io_errors() {
    let result = WriteFile
        .call(r#"{"file_path": "/nonexistent-dir-xyz/f", "file_content": "x"}"#)
        .await;
    assert!(result.contains("can't write file"), "result: {result}");
}

#[tokio::test]
async fn run_command_returns_exit_code_and_output() {
    let ok = RunCommand.call(r#"{"command": "echo hello"}"#).await;
    assert!(ok.contains("exit code: 0"), "result: {ok}");
    assert!(ok.contains("hello"), "result: {ok}");

    let fail = RunCommand.call(r#"{"command": "exit 3"}"#).await;
    assert!(fail.contains("exit code: 3"), "result: {fail}");
}

#[tokio::test]
async fn invalid_arguments_are_reported_as_text() {
    let result = RunCommand.call("not json").await;
    assert!(result.contains("invalid arguments"), "result: {result}");
}

#[tokio::test]
async fn builtin_registry_registers_all_tools() {
    let registry = builtin_registry();
    for name in ["read_file", "write_file", "delete_file", "run_command"] {
        assert!(registry.contains(name), "missing tool: {name}");
    }
    assert_eq!(registry.definitions().len(), 4);

    // 经注册表分发执行
    let path = temp_file("via-registry");
    let write = registry
        .call(
            "write_file",
            &format!("{{\"file_path\": \"{path}\", \"file_content\": \"via registry\"}}"),
        )
        .await
        .unwrap();
    assert!(write.contains("wrote"), "write result: {write}");
    let content = registry
        .call("read_file", &format!("{{\"file_path\": \"{path}\"}}"))
        .await
        .unwrap();
    assert_eq!(content, "via registry");

    DeleteFile
        .call(&format!("{{\"file_path\": \"{path}\"}}"))
        .await;
}
