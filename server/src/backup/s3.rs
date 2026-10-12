//! 最小 S3 兼容客户端（AWS SigV4）：PUT / GET / DELETE / ListObjectsV2。
//! 适用于 AWS S3、Cloudflare R2、MinIO、阿里云 OSS / 腾讯云 COS 的 S3 兼容接口。

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 对象存储连接信息（`[backup.s3]`）。密钥可明文或从环境变量读。
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct S3Config {
    /// 如 `https://s3.us-east-1.amazonaws.com`、`https://<account>.r2.cloudflarestorage.com`、`http://127.0.0.1:9000`。
    pub endpoint: String,
    /// 如 `us-east-1`；R2 用 `auto`。
    #[serde(default = "default_region")]
    pub region: String,
    pub bucket: String,
    /// 对象名前缀，如 `thought/`。
    #[serde(default)]
    pub prefix: String,
    #[serde(default, skip_serializing)]
    pub access_key: Option<String>,
    #[serde(default)]
    pub access_key_env: Option<String>,
    #[serde(default, skip_serializing)]
    pub secret_key: Option<String>,
    #[serde(default)]
    pub secret_key_env: Option<String>,
    /// 路径式寻址（`endpoint/bucket/key`）。MinIO 等需要；AWS 虚拟主机式填 false。
    #[serde(default = "default_true")]
    pub path_style: bool,
}

fn default_region() -> String {
    "us-east-1".into()
}

fn default_true() -> bool {
    true
}

/// 列表里的一个对象。
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub last_modified: String,
}

pub struct S3 {
    cfg: S3Config,
    access: String,
    secret: String,
    http: reqwest::Client,
}

type HmacSha256 = Hmac<Sha256>;

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn sha_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// RFC 3986 编码（SigV4 要求；`/` 在路径里保留）。
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "-_.~".contains(c) || (keep_slash && c == '/') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// UTC 时间 → (`YYYYMMDD`, `YYYYMMDDTHHMMSSZ`)。
fn amz_dates(ms: i64) -> (String, String) {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let date = format!("{y:04}{m:02}{d:02}");
    (date.clone(), format!("{date}T{:02}{:02}{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl S3 {
    pub fn new(cfg: S3Config) -> Result<Self, String> {
        let pick = |plain: &Option<String>, env: &Option<String>, what: &str| {
            plain
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| env.as_deref().and_then(|v| std::env::var(v).ok()))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("backup: {what} is not set"))
        };
        let access = pick(&cfg.access_key, &cfg.access_key_env, "access_key / access_key_env")?;
        let secret = pick(&cfg.secret_key, &cfg.secret_key_env, "secret_key / secret_key_env")?;
        if cfg.endpoint.is_empty() || cfg.bucket.is_empty() {
            return Err("backup: endpoint and bucket are required".into());
        }
        Ok(Self { cfg, access, secret, http: reqwest::Client::new() })
    }

    pub fn full_key(&self, name: &str) -> String {
        format!("{}{name}", self.cfg.prefix)
    }

    /// (url, host, canonical path)
    fn locate(&self, key: &str) -> Result<(String, String, String), String> {
        let ep = self.cfg.endpoint.trim_end_matches('/');
        let (scheme, rest) = ep.split_once("://").ok_or("backup: endpoint must start with http:// or https://")?;
        let enc_key = uri_encode(key, true);
        if self.cfg.path_style {
            let path = format!("/{}/{}", self.cfg.bucket, enc_key);
            Ok((format!("{scheme}://{rest}{path}"), rest.to_owned(), path))
        } else {
            let host = format!("{}.{rest}", self.cfg.bucket);
            let path = format!("/{enc_key}");
            Ok((format!("{scheme}://{host}{path}"), host, path))
        }
    }

    /// 签名并发送一个请求。`query` 为已排序、已编码的 `k=v&...`。
    async fn send(&self, method: reqwest::Method, key: &str, query: &str, body: Vec<u8>) -> Result<reqwest::Response, String> {
        let (url, host, path) = self.locate(key)?;
        let url = if query.is_empty() { url } else { format!("{url}?{query}") };
        let (date, stamp) = amz_dates(now_ms());
        let payload_hash = sha_hex(&body);
        let canonical = format!(
            "{method}\n{path}\n{query}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{stamp}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload_hash}"
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.cfg.region);
        let to_sign = format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", sha_hex(canonical.as_bytes()));
        let k = hmac(format!("AWS4{}", self.secret).as_bytes(), &date);
        let k = hmac(&k, &self.cfg.region);
        let k = hmac(&k, "s3");
        let k = hmac(&k, "aws4_request");
        let signature = hex::encode(hmac(&k, &to_sign));
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={signature}",
            self.access
        );
        let resp = self
            .http
            .request(method, url)
            .header("x-amz-date", stamp)
            .header("x-amz-content-sha256", payload_hash)
            .header("authorization", auth)
            .body(body)
            .send()
            .await
            .map_err(|e| format!("object storage unreachable: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let code = text.split("<Code>").nth(1).and_then(|t| t.split("</Code>").next()).unwrap_or("");
            return Err(format!("object storage returned {status} {code}"));
        }
        Ok(resp)
    }

    pub async fn put(&self, name: &str, body: Vec<u8>) -> Result<(), String> {
        self.send(reqwest::Method::PUT, &self.full_key(name), "", body).await.map(|_| ())
    }

    pub async fn get(&self, name: &str) -> Result<Vec<u8>, String> {
        let resp = self.send(reqwest::Method::GET, &self.full_key(name), "", Vec::new()).await?;
        resp.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
    }

    pub async fn delete(&self, name: &str) -> Result<(), String> {
        self.send(reqwest::Method::DELETE, &self.full_key(name), "", Vec::new()).await.map(|_| ())
    }

    /// 列出前缀下的对象（名字已去掉前缀）。
    pub async fn list(&self) -> Result<Vec<Object>, String> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut params = vec![("list-type".to_owned(), "2".to_owned()), ("prefix".to_owned(), self.cfg.prefix.clone())];
            if let Some(t) = &token {
                params.push(("continuation-token".into(), t.clone()));
            }
            params.sort();
            let query = params.iter().map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false))).collect::<Vec<_>>().join("&");
            // 列桶：key 为空，路径式为 /bucket/
            let resp = self.send(reqwest::Method::GET, "", &query, Vec::new()).await?;
            let xml = resp.text().await.map_err(|e| e.to_string())?;
            for item in xml.split("<Contents>").skip(1) {
                let field = |tag: &str| item.split(&format!("<{tag}>")).nth(1).and_then(|t| t.split(&format!("</{tag}>")).next()).unwrap_or("").to_owned();
                let key = field("Key");
                out.push(Object {
                    key: key.strip_prefix(&self.cfg.prefix).unwrap_or(&key).to_owned(),
                    size: field("Size").parse().unwrap_or(0),
                    last_modified: field("LastModified"),
                });
            }
            let truncated = xml.contains("<IsTruncated>true</IsTruncated>");
            token = xml.split("<NextContinuationToken>").nth(1).and_then(|t| t.split("</NextContinuationToken>").next()).map(str::to_owned);
            if !truncated || token.is_none() {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigv4_matches_aws_reference_signing_key() {
        // AWS 文档示例：secret "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY"，20120215 us-east-1 iam
        let k = hmac(b"AWS4wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "20120215");
        let k = hmac(&k, "us-east-1");
        let k = hmac(&k, "iam");
        let k = hmac(&k, "aws4_request");
        assert_eq!(hex::encode(k), "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d");
    }

    #[test]
    fn dates_and_encoding() {
        assert_eq!(amz_dates(1_791_676_800_000 + 3_723_000), ("20261011".into(), "20261011T010203Z".into()));
        assert_eq!(uri_encode("a b/c+d", true), "a%20b/c%2Bd");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
    }
}
