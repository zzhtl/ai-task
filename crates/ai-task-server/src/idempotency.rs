//! `Idempotency-Key` 的 HTTP 侧。
//!
//! 做成 extractor + handler 里调用的助手，**不做 tower layer**：
//! layer 进不到 handler 的事务里，而 ADR 0002 要求的恰恰是
//! 「响应与副作用写在同一个事务里，重放返回存下来的响应」。

use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::error::AppError;

/// 带幂等键的 JSON 报文。
///
/// 哈希算的是**反序列化之前的原始字节**：等反序列化完再哈希，
/// 字段顺序、空白、未知字段的差异就被抹平了，而那些差异恰恰说明
/// 客户端发的不是同一个请求。
pub struct IdempotentJson<T> {
    pub body: T,
    /// 客户端给的键。没给就是 `None`——这个头不是必需的。
    pub key: Option<String>,
    /// 原始字节的 SHA-256，十六进制。
    pub hash: String,
}

impl<S, T> FromRequest<S> for IdempotentJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = Response;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let key = read_key(request.headers()).map_err(IntoResponse::into_response)?;

        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(IntoResponse::into_response)?;

        let hash = hex(Sha256::digest(&bytes));

        // 空 body 当成 `null`：TriggerRun 的字段全是可选的，
        // 前端"直接运行"时不带 body 是合法的。
        let slice: &[u8] = if bytes.is_empty() { b"null" } else { &bytes };
        let body = serde_json::from_slice(slice)
            .map_err(|err| AppError::BadRequest(format!("报文解析失败：{err}")).into_response())?;

        Ok(Self { body, key, hash })
    }
}

/// 键的长度上限。够长到能放 UUID，短到不会被人拿来当存储用。
const MAX_KEY_LEN: usize = 200;

fn read_key(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let Some(raw) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = raw
        .to_str()
        .map_err(|_| AppError::BadRequest("Idempotency-Key 不是合法的 ASCII".into()))?
        .trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_KEY_LEN {
        return Err(AppError::BadRequest(format!(
            "Idempotency-Key 超过 {MAX_KEY_LEN} 字符"
        )));
    }
    Ok(Some(value.to_owned()))
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut acc, byte| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{byte:02x}");
            acc
        })
}

/// 重放一条存下来的响应。
pub fn replay(status: i32, body: serde_json::Value) -> Response {
    let code = u16::try_from(status)
        .ok()
        .and_then(|c| StatusCode::from_u16(c).ok())
        .unwrap_or(StatusCode::OK);
    // Location 不入库：从响应体里的 run id 重建，省得存一份会过期的头
    let location = body
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|id| format!("/api/v1/runs/{id}").parse().ok());

    let mut headers = HeaderMap::new();
    if let Some(location) = location {
        headers.insert(axum::http::header::LOCATION, location);
    }
    // 让客户端知道这是重放而不是新做的一次
    headers.insert(
        "idempotency-replayed",
        axum::http::HeaderValue::from_static("true"),
    );
    (code, headers, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_bytes_hash_the_same_and_different_bytes_do_not() {
        let a = hex(Sha256::digest(br#"{"dry_run":true}"#));
        let b = hex(Sha256::digest(br#"{"dry_run":true}"#));
        let c = hex(Sha256::digest(br#"{"dry_run":false}"#));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn whitespace_differences_are_a_different_request() {
        // 故意的：哈希算的是原始字节。序列化差异说明客户端发的不是同一个东西，
        // 把它当成同一个请求去重放，等于悄悄丢掉一次真实的调用。
        let a = hex(Sha256::digest(br#"{"dry_run":true}"#));
        let b = hex(Sha256::digest(br#"{ "dry_run": true }"#));
        assert_ne!(a, b);
    }

    #[test]
    fn a_blank_key_is_treated_as_absent() {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", "   ".parse().expect("header"));
        assert!(read_key(&headers).expect("read").is_none());
    }

    #[test]
    fn an_overlong_key_is_rejected_rather_than_truncated() {
        let mut headers = HeaderMap::new();
        let long = "x".repeat(MAX_KEY_LEN + 1);
        headers.insert("idempotency-key", long.parse().expect("header"));
        assert!(read_key(&headers).is_err());
    }
}
