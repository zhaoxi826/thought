//! 静态前端托管与安全响应头。

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use std::path::Path;
use tower_http::services::{ServeDir, ServeFile};

/// SPA：命中文件直接返回，否则兜底 `index.html`（200，便于前端路由深链）。
pub(super) fn router(web_dir: &Path) -> Router {
    if !web_dir.is_dir() {
        tracing::warn!(web_dir = %web_dir.display(), "web dir does not exist; static frontend will 404");
    }
    let spa = ServeDir::new(web_dir).fallback(ServeFile::new(web_dir.join("index.html")));
    Router::new().fallback_service(spa)
}

/// 所有响应加安全头。CSP 只允许同源资源；SSE / API 走 fetch，同源即可。
pub(super) async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let set = |headers: &mut axum::http::HeaderMap, name, value: &'static str| {
        headers.entry(name).or_insert(HeaderValue::from_static(value));
    };
    set(
        headers,
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
    );
    set(headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(headers, header::REFERRER_POLICY, "no-referrer");
    set(headers, header::X_FRAME_OPTIONS, "DENY");
    response
}
