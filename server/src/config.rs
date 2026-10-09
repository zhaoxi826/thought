//! 后端配置（通常来自 `thought.toml`）。

use agent::AgentConfig;
use model::ModelConfig;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use symbol::MemoryConfig;

/// 整个后端的配置。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ServerConfig {
    #[serde(default)]
    pub http: HttpConfig,

    pub auth: AuthConfig,

    /// 后端自身的数据存储（用户、会话历史）。与记忆库可共用同一个 SurrealKV 文件。
    #[serde(default)]
    pub storage: StorageConfig,

    /// 模型池：名字 → 配置。agent 配置按名字引用。
    pub models: BTreeMap<String, ModelConfig>,

    /// 向量化模型（可选）；缺省时记忆检索只用全文 + 图通道。
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,

    /// 记忆器；缺省时不启用记忆。
    #[serde(default)]
    pub memory: Option<MemorySection>,

    pub agent: AgentConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HttpConfig {
    /// 监听地址。默认只监听本机，对外暴露请放在反向代理（TLS）之后。
    #[serde(default = "default_bind")]
    pub bind: String,

    /// 静态前端目录，由服务器同源托管在 `/`。相对路径见 [`ServerConfig::resolve_paths`]。
    #[serde(default = "default_web_dir")]
    pub web_dir: PathBuf,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            web_dir: default_web_dir(),
        }
    }
}

fn default_bind() -> String {
    "127.0.0.1:8080".to_owned()
}

fn default_web_dir() -> PathBuf {
    PathBuf::from("web")
}

impl ServerConfig {
    /// 把相对路径（`http.web_dir`、`surrealkv://` 存储路径）改为相对 `base`
    /// （通常是配置文件所在目录），这样从任意工作目录启动结果都一致。
    pub fn resolve_paths(&mut self, base: &Path) {
        if self.http.web_dir.is_relative() {
            self.http.web_dir = base.join(&self.http.web_dir);
        }
        resolve_endpoint(&mut self.storage.endpoint, base);
        if let Some(memory) = &mut self.memory {
            resolve_endpoint(&mut memory.store.endpoint, base);
        }
    }
}

/// 配置文件所在目录（`thought.toml` → `.`）。
pub fn config_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// `surrealkv://relative/path` → `surrealkv://<base>/relative/path`；其他协议不动。
fn resolve_endpoint(endpoint: &mut String, base: &Path) {
    const SCHEME: &str = "surrealkv://";
    if let Some(path) = endpoint.strip_prefix(SCHEME)
        && Path::new(path).is_relative()
    {
        *endpoint = format!("{SCHEME}{}", base.join(path).display());
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AuthConfig {
    /// HS256 签名密钥，至少 32 字节。与 `jwt_secret_env` 二选一。
    #[serde(default, skip_serializing)]
    pub jwt_secret: Option<String>,

    #[serde(default)]
    pub jwt_secret_env: Option<String>,

    /// token 有效期（秒），默认 12 小时。
    #[serde(default = "default_ttl")]
    pub token_ttl_secs: u64,

    /// 首次启动且没有任何用户时创建的管理员。密码可从环境变量读。
    #[serde(default)]
    pub bootstrap_admin: Option<BootstrapAdmin>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BootstrapAdmin {
    pub username: String,
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    #[serde(default)]
    pub password_env: Option<String>,
}

fn default_ttl() -> u64 {
    12 * 3600
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub namespace: String,
    pub database: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            endpoint: "surrealkv://data/thought".to_owned(),
            namespace: "thought".to_owned(),
            database: "server".to_owned(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EmbeddingConfig {
    /// 含版本前缀，如 `https://api.openai.com/v1`。
    pub base_url: String,
    pub model: String,
    pub dimension: usize,
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// 是否在请求里带 `dimensions`（只有支持可变维度的模型需要）。
    #[serde(default)]
    pub send_dimensions: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MemorySection {
    #[serde(flatten)]
    pub store: MemoryConfig,

    /// 抽取三元组用的模型名（模型池里的 key）；缺省不抽取。
    #[serde(default)]
    pub extractor_model: Option<String>,

    /// 每个用户一个记忆空间（database = `user_<id>`）；否则所有用户共用。
    #[serde(default = "default_true")]
    pub per_user: bool,

    /// 是否把每条用户消息自动写入记忆。
    #[serde(default)]
    pub save_user_messages: bool,
}

fn default_true() -> bool {
    true
}

/// 从环境变量或明文取密钥。
pub(crate) fn secret(plain: &Option<String>, env: &Option<String>) -> Option<String> {
    plain
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| env.as_deref().and_then(|v| std::env::var(v).ok()))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_paths_against_config_dir() {
        let mut endpoint = "surrealkv://data/thought".to_owned();
        resolve_endpoint(&mut endpoint, Path::new("/etc/thought"));
        assert_eq!(endpoint, "surrealkv:///etc/thought/data/thought");

        for fixed in ["surrealkv:///abs/db", "mem://"] {
            let mut e = fixed.to_owned();
            resolve_endpoint(&mut e, Path::new("/etc/thought"));
            assert_eq!(e, fixed);
        }
    }
}
