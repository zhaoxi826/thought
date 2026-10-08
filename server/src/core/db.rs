//! 后端自身的数据：用户与会话（含持久化的主控对话历史）。

use crate::core::auth::{hash_password, Role};
use crate::core::error::ApiError;
use model::Message;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use surrealdb::engine::any::Any;
use surrealdb::Surreal;

const SCHEMA: &str = "
DEFINE TABLE IF NOT EXISTS user SCHEMALESS;
DEFINE INDEX IF NOT EXISTS user_name ON user FIELDS username UNIQUE;
DEFINE TABLE IF NOT EXISTS chat_session SCHEMALESS;
DEFINE INDEX IF NOT EXISTS chat_session_owner ON chat_session FIELDS owner;
";

#[derive(Clone)]
pub struct Db {
    db: Surreal<Any>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct UserRow {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
}

/// 对外的用户视图（不含密码哈希）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct UserView {
    pub id: String,
    pub username: String,
    pub role: Role,
}

impl From<UserRow> for UserView {
    fn from(u: UserRow) -> Self {
        Self {
            id: u.id,
            username: u.username,
            role: u.role,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SessionView {
    pub id: String,
    pub title: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    /// 在给定连接上切到 `namespace/database` 并建表。
    pub async fn new(conn: &Surreal<Any>, namespace: &str, database: &str) -> Result<Self, surrealdb::Error> {
        let db = conn.clone();
        db.use_ns(namespace).use_db(database).await?;
        db.query(SCHEMA).await?.check()?;
        Ok(Self { db })
    }

    async fn query<T: DeserializeOwned>(&self, sql: &str, vars: Json) -> Result<T, ApiError> {
        let mut response = self.db.query(sql).bind(vars).await?.check()?;
        let last = response.num_statements().saturating_sub(1);
        let value: surrealdb::types::Value = response.take(last)?;
        Ok(serde_json::from_value(value.into_json_value())?)
    }

    // ---- 用户 ----

    pub async fn user_count(&self) -> Result<u64, ApiError> {
        self.query("RETURN count(SELECT id FROM user)", json!({})).await
    }

    /// 创建用户；用户名已存在时返回 409。
    pub async fn create_user(&self, username: &str, password: &str, role: Role) -> Result<UserView, ApiError> {
        validate_username(username)?;
        if password.chars().count() < 8 {
            return Err(ApiError::bad_request("password must be at least 8 characters"));
        }
        if self.find_user(username).await?.is_some() {
            return Err(ApiError::conflict("username already exists"));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let rows: Vec<UserRow> = self
            .query(
                "CREATE type::record('user', $id) CONTENT $data
                 RETURN meta::id(id) AS id, username, password_hash, role",
                json!({
                    "id": id,
                    "data": {
                        "username": username,
                        "password_hash": hash_password(password)?,
                        "role": role,
                        "created_at_ms": now_ms(),
                    }
                }),
            )
            .await
            // 并发创建时由唯一索引兜底
            .map_err(|e| if e.status.is_server_error() { ApiError::conflict("username already exists") } else { e })?;
        rows.into_iter()
            .next()
            .map(UserView::from)
            .ok_or_else(|| ApiError::internal("create user returned nothing"))
    }

    pub async fn find_user(&self, username: &str) -> Result<Option<UserRow>, ApiError> {
        let rows: Vec<UserRow> = self
            .query(
                "SELECT meta::id(id) AS id, username, password_hash, role FROM user
                 WHERE username = $username LIMIT 1",
                json!({ "username": username }),
            )
            .await?;
        Ok(rows.into_iter().next())
    }

    pub async fn list_users(&self) -> Result<Vec<UserView>, ApiError> {
        let rows: Vec<UserRow> = self
            .query(
                "SELECT meta::id(id) AS id, username, password_hash, role FROM user ORDER BY username",
                json!({}),
            )
            .await?;
        Ok(rows.into_iter().map(UserView::from).collect())
    }

    // ---- 会话 ----

    pub async fn create_session(&self, owner: &str, title: &str) -> Result<SessionView, ApiError> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = now_ms();
        let rows: Vec<SessionView> = self
            .query(
                "CREATE type::record('chat_session', $id) CONTENT $data
                 RETURN meta::id(id) AS id, title, created_at_ms, updated_at_ms",
                json!({
                    "id": id,
                    "data": {
                        "owner": owner, "title": title, "history": [],
                        "created_at_ms": now, "updated_at_ms": now,
                    }
                }),
            )
            .await?;
        rows.into_iter()
            .next()
            .ok_or_else(|| ApiError::internal("create session returned nothing"))
    }

    pub async fn list_sessions(&self, owner: &str) -> Result<Vec<SessionView>, ApiError> {
        self.query(
            "SELECT meta::id(id) AS id, title, created_at_ms, updated_at_ms FROM chat_session
             WHERE owner = $owner ORDER BY updated_at_ms DESC",
            json!({ "owner": owner }),
        )
        .await
    }

    /// 读取会话（校验归属）；不存在或不属于该用户都返回 404，不泄露存在性。
    pub async fn get_session(&self, owner: &str, id: &str) -> Result<(SessionView, Vec<Message>), ApiError> {
        #[derive(Deserialize)]
        struct Row {
            #[serde(flatten)]
            view: SessionView,
            #[serde(default)]
            history: Vec<Message>,
        }
        let rows: Vec<Row> = self
            .query(
                "SELECT meta::id(id) AS id, title, created_at_ms, updated_at_ms, history
                 FROM type::record('chat_session', $id) WHERE owner = $owner",
                json!({ "id": id, "owner": owner }),
            )
            .await?;
        let row = rows.into_iter().next().ok_or_else(|| ApiError::not_found("session not found"))?;
        Ok((row.view, row.history))
    }

    /// 覆盖保存会话历史（主控每处理完一批输入调用一次）。
    pub async fn save_history(&self, id: &str, history: &[Message]) -> Result<(), ApiError> {
        let _: Json = self
            .query(
                "UPDATE type::record('chat_session', $id) SET history = $history, updated_at_ms = $now",
                json!({ "id": id, "history": history, "now": now_ms() }),
            )
            .await?;
        Ok(())
    }

    pub async fn delete_session(&self, owner: &str, id: &str) -> Result<bool, ApiError> {
        let rows: Vec<Json> = self
            .query(
                "DELETE type::record('chat_session', $id) WHERE owner = $owner RETURN BEFORE",
                json!({ "id": id, "owner": owner }),
            )
            .await?;
        Ok(!rows.is_empty())
    }
}

fn validate_username(name: &str) -> Result<(), ApiError> {
    let ok = (3..=32).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad_request("username must be 3-32 chars of [A-Za-z0-9_.-]"))
    }
}
