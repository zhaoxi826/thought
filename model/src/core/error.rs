//! 统一错误类型。

/// 统一错误类型。
///
/// 区分底层传输错误、服务端返回的错误状态（保留响应体便于排查）与
/// “响应结构异常”，避免把空回复静默当成成功。
#[derive(Debug)]
pub enum Error {
    /// 底层 reqwest 传输错误。
    Http(reqwest::Error),

    /// 服务端返回非 2xx 状态；`body` 为原始响应体（通常是厂商的错误 JSON）。
    Api { status: u16, body: String },

    /// 响应里没有任何可用内容。
    EmptyResponse,

    /// 响应数据无法按协议解析（例如流式事件体损坏）。
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "http error: {e}"),
            Error::Api { status, body } => write!(f, "api error {status}: {body}"),
            Error::EmptyResponse => write!(f, "response contained no content"),
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

/// 检查响应状态：非 2xx 时读出响应体，包成 [`Error::Api`]。
pub(crate) async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    Err(Error::Api {
        status: status.as_u16(),
        body,
    })
}
