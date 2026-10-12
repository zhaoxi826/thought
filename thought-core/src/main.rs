//! thought：组装并启动整个系统。
//!
//! ```text
//! thought [CONFIG]        # 默认 ./thought.toml
//! ```
//!
//! 读取配置 → 构造工具池（内置文件 / 命令工具）→ `server::build` 组装模型池、
//! agent 系统、记忆器与 HTTP 路由 → 启动记忆维护任务 → 监听，Ctrl-C 优雅退出。

use server::ServerConfig;
use std::process::ExitCode;
use std::time::Duration;

use tracing_subscriber::EnvFilter;

/// 记忆维护（抽取队列 + 衰减）的间隔。
const MEMORY_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("restore") {
        return restore(&args[1..]).await;
    }
    let path = args.first().cloned().unwrap_or_else(|| "thought.toml".to_owned());
    // 从文件组装：相对路径以配置文件所在目录为准；内置文件 / 命令工具经 [sandbox] 隔离
    let built = server::build_from_file(std::path::Path::new(&path), plugin::Registry::new())
        .await
        .map_err(|e| e.to_string())?;
    let config = built.state.config();
    // 维护任务总是启动：记忆整理（有记忆时）+ 回收站 / 备份过期清理
    server::spawn_memory_worker(built.state.clone(), MEMORY_MAINTENANCE_INTERVAL);
    let _ = &config;

    let listener = tokio::net::TcpListener::bind(&config.http.bind)
        .await
        .map_err(|e| format!("bind {}: {e}", config.http.bind))?;
    tracing::info!("listening on http://{}", config.http.bind);
    axum::serve(listener, built.router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutting down");
        })
        .await
        .map_err(|e| format!("serve: {e}"))
}


/// `thought restore <备份名> [CONFIG]`：从对象存储恢复。须先停服务（SurrealKV 文件只能被一个进程打开）。
/// 不带备份名时列出可用的备份。
async fn restore(args: &[String]) -> Result<(), String> {
    let (name, path) = match args {
        [] => (None, "thought.toml".to_owned()),
        [n] if n.ends_with(".toml") => (None, n.clone()),
        [n] => (Some(n.clone()), "thought.toml".to_owned()),
        [n, p, ..] => (Some(n.clone()), p.clone()),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}"))?;
    let mut config: ServerConfig = toml::from_str(&text).map_err(|e| format!("parse {path}: {e}"))?;
    config.resolve_paths(&server::config::config_dir(std::path::Path::new(&path)));
    let backup = config.backup.clone().ok_or("no [backup] section in config")?;
    let Some(name) = name else {
        let client = server::backup::s3::S3::new(backup.s3.clone())?;
        let mut items = client.list().await?;
        items.sort_by(|a, b| b.key.cmp(&a.key));
        println!("可用的备份（新的在前）：");
        for o in items.iter().filter(|o| o.key.starts_with("thought-")) {
            println!("  {}  {} 字节  {}", o.key, o.size, o.last_modified);
        }
        println!("\n恢复：thought restore <备份名> {path}（先停服务）");
        return Ok(());
    };
    println!("将用备份 {name} 覆盖 {} 中的全部数据（她的意识流、记忆、自我、用户……）。", config.storage.endpoint);
    println!("请确认服务已停止。输入 yes 继续：");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| e.to_string())?;
    if answer.trim() != "yes" {
        return Err("cancelled".into());
    }
    let n = server::backup::restore(&config.storage.endpoint, &backup, &name).await?;
    println!("已恢复 {n} 个数据库。可以启动服务了。");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn example_config_parses() {
        let text = include_str!("../../thought.example.toml");
        let config: server::ServerConfig = toml::from_str(text).expect("example config should parse");
        assert!(config.models.contains_key("deepseek"));
        assert_eq!(config.agent.workers[0].name, "coder");
    }
}
