//! thought：组装并启动整个系统。
//!
//! ```text
//! thought [CONFIG]        # 默认 ./thought.toml
//! ```
//!
//! 读取配置 → 构造工具池（内置文件 / 命令工具）→ `server::build` 组装模型池、
//! agent 系统、记忆器与 HTTP 路由 → 启动记忆维护任务 → 监听，Ctrl-C 优雅退出。

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
    let path = std::env::args().nth(1).unwrap_or_else(|| "thought.toml".to_owned());
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
