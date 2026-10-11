//! 配置读写：脱敏输出、ETag、校验后原子写回、热生效。

use crate::config::ServerConfig;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::http::StatusCode;
use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::path::Path;

/// 写回结果。
#[derive(Serialize, Debug, PartialEq)]
pub struct ApplyResult {
    pub applied: bool,
    pub restart_required: Vec<&'static str>,
    pub hot_applied: Vec<&'static str>,
}

/// 配置的对外形态：`ServerConfig` 序列化本身就跳过所有明文密钥字段
/// （`api_key` / `jwt_secret` / `password` 均为 `skip_serializing`）。
pub fn public_json(config: &ServerConfig) -> Result<Json, ApiError> {
    Ok(serde_json::to_value(config)?)
}

/// 以脱敏 JSON 的规范化字节算 ETag：密钥变化不会体现在 ETag 里，但任何可见字段变化都会。
pub fn etag(config: &ServerConfig) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&public_json(config)?)?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("\"{}\"", digest[..8].iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

/// 脱敏 TOML（`?format=toml`）。
pub fn public_toml(config: &ServerConfig) -> Result<String, ApiError> {
    toml::to_string_pretty(config).map_err(ApiError::internal)
}

/// 把旧配置里的明文密钥补回新配置：前端拿到的是脱敏结构，原样写回不应清掉密钥。
fn preserve_secrets(new: &mut ServerConfig, old: &ServerConfig) {
    if new.auth.jwt_secret.is_none() {
        new.auth.jwt_secret = old.auth.jwt_secret.clone();
    }
    if let (Some(n), Some(o)) = (&mut new.auth.bootstrap_admin, &old.auth.bootstrap_admin)
        && n.password.is_none()
    {
        n.password = o.password.clone();
    }
    for (name, model) in &mut new.models {
        if model.api_key.is_none()
            && let Some(o) = old.models.get(name)
        {
            model.api_key = o.api_key.clone();
        }
    }
    if let (Some(n), Some(o)) = (&mut new.embedding, &old.embedding)
        && n.api_key.is_none()
    {
        n.api_key = o.api_key.clone();
    }
}

fn changed<T: Serialize>(a: &T, b: &T) -> bool {
    serde_json::to_value(a).ok() != serde_json::to_value(b).ok()
}

/// 比较新旧配置，划分需重启 / 已热生效的节。
fn classify(new: &ServerConfig, old: &ServerConfig) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut restart = Vec::new();
    let mut hot = Vec::new();
    if changed(&new.http, &old.http) {
        restart.push("http");
    }
    if changed(&new.storage, &old.storage) {
        restart.push("storage");
    }
    if changed(&new.auth, &old.auth)
        || new.auth.jwt_secret != old.auth.jwt_secret
    {
        restart.push("auth");
    }
    if changed(&new.embedding, &old.embedding) {
        restart.push("embedding");
    }
    if changed(&new.sandbox, &old.sandbox) {
        restart.push("sandbox");
    }
    if changed(&new.mind, &old.mind) {
        restart.push("mind");
    }
    // memory：只有 per_user / save_user_messages 热生效，其余需重启
    let mem_core = |c: &ServerConfig| {
        c.memory.as_ref().map(|m| {
            let mut v = serde_json::to_value(m).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.remove("per_user");
                o.remove("save_user_messages");
            }
            v
        })
    };
    if mem_core(new) != mem_core(old) {
        restart.push("memory");
    } else if changed(&new.memory, &old.memory) {
        hot.push("memory");
    }
    if changed(&new.models, &old.models) {
        hot.push("models");
    }
    if changed(&new.agent.controller, &old.agent.controller) {
        hot.push("agent.controller");
    }
    if changed(&new.agent.workers, &old.agent.workers) {
        hot.push("agent.workers");
    }
    if changed(&new.agent.limits, &old.agent.limits) {
        hot.push("agent.limits");
    }
    if changed(&new.budget, &old.budget) {
        hot.push("budget");
    }
    (restart, hot)
}

/// 先写临时文件再原子替换，旧文件留一份 `.bak`。
fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    if path.exists() {
        std::fs::copy(path, path.with_extension("toml.bak"))?;
    }
    std::fs::rename(&tmp, path)
}

/// 校验并写回配置。`body` 为前端提交的脱敏结构（全量）。
pub fn apply(state: &AppState, if_match: Option<&str>, body: Json) -> Result<ApplyResult, ApiError> {
    let path = state
        .inner
        .config_path
        .as_ref()
        .ok_or_else(|| ApiError::not_found("config file not available"))?;

    // 写回整体串行化：持写锁完成 校验 → 写文件 → 替换运行时
    let mut current = state.inner.config.write().expect("config lock poisoned");
    let current_tag = etag(&current)?;
    match if_match {
        Some(tag) if tag.trim() == current_tag || tag.trim() == "*" => {}
        Some(_) => {
            return Err(ApiError::new(StatusCode::PRECONDITION_FAILED, "config changed, reload first"));
        }
        None => return Err(ApiError::new(StatusCode::PRECONDITION_REQUIRED, "If-Match header required")),
    }

    let unprocessable = |m: String| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, m);
    let mut new: ServerConfig = serde_json::from_value(body).map_err(|e| unprocessable(e.to_string()))?;
    // GET 展示的是相对路径，这里解析回与运行时一致的绝对路径再比较
    new.resolve_paths(&crate::config::config_dir(path));
    preserve_secrets(&mut new, &current);

    // 语义校验
    new.http
        .bind
        .parse::<std::net::SocketAddr>()
        .map_err(|e| unprocessable(format!("http.bind: {e}")))?;
    if let Some(m) = &new.memory
        && let Some(name) = &m.extractor_model
        && !new.models.contains_key(name)
    {
        return Err(unprocessable(format!("memory.extractor_model: unknown model {name:?}")));
    }
    // 构造一遍模型池与 agent 系统：模型引用、工具名、密钥可解析性都在这里校验
    let agents = crate::build_agents(&new, &state.inner.tools).map_err(|e| unprocessable(e.0))?;

    let (restart_required, hot_applied) = classify(&new, &current);

    // 文件里保留用户写的相对路径：写盘的是“去掉解析结果”的版本
    let mut on_disk = new.clone();
    unresolve_paths(&mut on_disk, &crate::config::config_dir(path));
    let mut text = toml::to_string_pretty(&on_disk).map_err(ApiError::internal)?;
    restore_plain_secrets(&mut text, &on_disk);
    atomic_write(path, &text).map_err(ApiError::internal)?;

    // 热生效：新会话使用新 agent 系统；memory 的可热字段立即生效
    *state.inner.agents.write().expect("agents lock poisoned") = agents;
    *state.inner.memory_cfg.write().expect("memory cfg lock poisoned") = new.memory.clone();
    state.inner.budget.set_config(new.budget.clone());
    *current = new;

    Ok(ApplyResult {
        applied: true,
        restart_required,
        hot_applied,
    })
}

/// `resolve_paths` 的逆操作：把落在配置目录下的绝对路径还原为相对路径。
fn unresolve_paths(config: &mut ServerConfig, base: &Path) {
    if let Ok(rel) = config.http.web_dir.strip_prefix(base) {
        config.http.web_dir = rel.to_path_buf();
    }
    let sb = &mut config.sandbox;
    for p in [&mut sb.workspace, &mut sb.trash, &mut sb.backup].into_iter().chain(sb.protected.iter_mut()) {
        if let Ok(rel) = p.strip_prefix(base) {
            *p = rel.to_path_buf();
        }
    }
    let strip = |endpoint: &mut String| {
        const SCHEME: &str = "surrealkv://";
        if let Some(p) = endpoint.strip_prefix(SCHEME)
            && let Ok(rel) = Path::new(p).strip_prefix(base)
        {
            *endpoint = format!("{SCHEME}{}", rel.display());
        }
    };
    strip(&mut config.storage.endpoint);
    if let Some(m) = &mut config.memory {
        strip(&mut m.store.endpoint);
    }
}

/// 序列化会跳过明文密钥；若原文件里用的是明文（而非 `*_env`），写回时要把它们补上，
/// 否则重启后密钥丢失。以 TOML 追加键的方式补回对应表。
fn restore_plain_secrets(text: &mut String, config: &ServerConfig) {
    let mut doc: toml::Table = match toml::from_str(text) {
        Ok(d) => d,
        Err(_) => return,
    };
    let set = |doc: &mut toml::Table, path: &[&str], key: &str, value: &Option<String>| {
        let Some(v) = value else { return };
        let mut table = &mut *doc;
        for seg in path {
            table = match table.get_mut(*seg).and_then(|t| t.as_table_mut()) {
                Some(t) => t,
                None => return,
            };
        }
        table.insert(key.to_owned(), toml::Value::String(v.clone()));
    };
    set(&mut doc, &["auth"], "jwt_secret", &config.auth.jwt_secret);
    if let Some(admin) = &config.auth.bootstrap_admin {
        set(&mut doc, &["auth", "bootstrap_admin"], "password", &admin.password);
    }
    for (name, model) in &config.models {
        set(&mut doc, &["models", name], "api_key", &model.api_key);
    }
    if let Some(e) = &config.embedding {
        set(&mut doc, &["embedding"], "api_key", &e.api_key);
    }
    if let Ok(t) = toml::to_string_pretty(&doc) {
        *text = t;
    }
}

/// `GET /api/config` 的响应体。
pub fn read(state: &AppState) -> Result<(String, Json), ApiError> {
    if state.inner.config_path.is_none() {
        return Err(ApiError::not_found("config file not available"));
    }
    let config = state.inner.config.read().expect("config lock poisoned");
    let mut public = public_json(&config)?;
    // 展示用户写的相对路径，而不是解析后的绝对路径
    if let Some(path) = &state.inner.config_path {
        let mut shown = config.clone();
        unresolve_paths(&mut shown, &crate::config::config_dir(path));
        public = public_json(&shown)?;
    }
    Ok((etag(&config)?, public))
}

pub fn read_toml(state: &AppState) -> Result<String, ApiError> {
    let path = state
        .inner
        .config_path
        .as_ref()
        .ok_or_else(|| ApiError::not_found("config file not available"))?;
    let mut shown = state.inner.config.read().expect("config lock poisoned").clone();
    unresolve_paths(&mut shown, &crate::config::config_dir(path));
    public_toml(&shown)
}
