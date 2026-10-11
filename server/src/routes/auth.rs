//! 登录与当前用户。

use crate::core::auth::{AuthUser, verify_password};
use crate::core::db::UserView;
use crate::core::error::ApiError;
use crate::core::state::AppState;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
pub(super) struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub(super) struct LoginResponse {
    token: String,
    expires_at: u64,
    user: UserView,
}

pub(super) async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, ApiError> {
    if let Some(secs) = state.inner.login_limiter.locked(&req.username) {
        return Err(ApiError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            format!("too many failed attempts; try again in {secs}s"),
        ));
    }
    let user = state.db().find_user(&req.username).await?;
    // 用户不存在时也做一次哈希校验，避免通过响应时间探测用户名
    const DUMMY: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$7QjKqRZ6yEJfTOCLUL7t1cD3qNhEOHVGDl1b1mbVFkM";
    let ok = verify_password(
        &req.password,
        user.as_ref().map_or(DUMMY, |u| &u.password_hash),
    );
    let user = match user {
        Some(user) if ok => user,
        _ => {
            state.inner.login_limiter.failure(&req.username);
            return Err(ApiError::unauthorized("invalid username or password"));
        }
    };
    state.inner.login_limiter.success(&req.username);
    let (token, expires_at) = state.jwt.issue(&user.id, &user.username, user.role)?;
    Ok(Json(LoginResponse {
        token,
        expires_at,
        user: user.into(),
    }))
}

pub(super) async fn me(AuthUser(claims): AuthUser) -> Json<Value> {
    Json(json!({ "id": claims.sub, "username": claims.name, "role": claims.role }))
}

