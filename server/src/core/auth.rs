//! 认证：argon2 密码哈希 + HS256 JWT。

use crate::core::error::ApiError;
use crate::core::state::AppState;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// 角色。admin 可管理用户与查看模型配置。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    User,
}

/// JWT 载荷。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Claims {
    /// 用户 id。
    pub sub: String,
    pub name: String,
    pub role: Role,
    pub iat: u64,
    pub exp: u64,
}

/// 签发 / 校验 token 的密钥对。
pub struct Jwt {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    ttl_secs: u64,
}

/// 密钥最短长度（字节）。HS256 要求密钥至少与哈希输出等长。
pub const MIN_SECRET_LEN: usize = 32;

impl Jwt {
    pub fn new(secret: &[u8], ttl_secs: u64) -> Result<Self, String> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(format!("jwt secret must be at least {MIN_SECRET_LEN} bytes"));
        }
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 30;
        Ok(Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
            ttl_secs,
        })
    }

    pub fn issue(&self, user_id: &str, name: &str, role: Role) -> Result<(String, u64), ApiError> {
        let iat = jsonwebtoken::get_current_timestamp();
        let claims = Claims {
            sub: user_id.to_owned(),
            name: name.to_owned(),
            role,
            iat,
            exp: iat + self.ttl_secs,
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| ApiError::internal(format!("sign token: {e}")))?;
        Ok((token, claims.exp))
    }

    pub fn verify(&self, token: &str) -> Result<Claims, ApiError> {
        decode::<Claims>(token, &self.decoding, &self.validation)
            .map(|data| data.claims)
            .map_err(|_| ApiError::unauthorized("invalid or expired token"))
    }
}

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| ApiError::internal(format!("hash password: {e}")))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

/// 已认证用户：从 `Authorization: Bearer <token>` 提取并校验。
#[derive(Debug, Clone)]
pub struct AuthUser(pub Claims);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        Ok(AuthUser(state.jwt.verify(token.trim())?))
    }
}

/// 仅限管理员。
#[derive(Debug, Clone)]
pub struct AdminUser(pub Claims);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let AuthUser(claims) = AuthUser::from_request_parts(parts, state).await?;
        if claims.role != Role::Admin {
            return Err(ApiError::forbidden("admin only"));
        }
        Ok(AdminUser(claims))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn jwt_roundtrip_and_rejections() {
        let jwt = Jwt::new(&[7u8; 32], 60).unwrap();
        let (token, _) = jwt.issue("u1", "alice", Role::User).unwrap();
        let claims = jwt.verify(&token).unwrap();
        assert_eq!((claims.sub.as_str(), claims.role), ("u1", Role::User));

        let other = Jwt::new(&[8u8; 32], 60).unwrap();
        assert!(other.verify(&token).is_err(), "wrong key");
        assert!(jwt.verify(&format!("{token}x")).is_err(), "tampered");
        assert!(Jwt::new(b"short", 60).is_err());

        let expired = Jwt { ttl_secs: 0, ..Jwt::new(&[7u8; 32], 0).unwrap() };
        let mut claims = jwt.verify(&token).unwrap();
        claims.exp = claims.iat - 120;
        let stale = encode(&Header::new(Algorithm::HS256), &claims, &expired.encoding).unwrap();
        assert!(jwt.verify(&stale).is_err(), "expired");
    }
}
