//! 请求 ID 的生成与传播。
//!
//! ID 同时出现在三个地方，方便把一次报错串起来：响应头 `x-request-id`、
//! 错误响应体的 `request_id` 字段、以及该请求所有日志的 span 字段。
//!
//! 用 task-local 而不是把 id 塞进每个 handler 的签名：`IntoResponse` 拿不到
//! 请求上下文，而错误响应体里必须带上它。

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;

pub const HEADER: &str = "x-request-id";

tokio::task_local! {
    static REQUEST_ID: String;
}

/// 当前请求的 ID。不在请求上下文里时返回 `"-"`。
#[must_use]
pub fn current() -> String {
    REQUEST_ID
        .try_with(Clone::clone)
        .unwrap_or_else(|_| "-".to_string())
}

/// 取入站的 `x-request-id`，没有就生成一个；出站时回写。
///
/// 生成用 UUIDv7 而不是 v4：日志按 ID 排序即按时间排序，翻日志时有用。
pub async fn layer(mut request: Request, next: Next) -> Response {
    let incoming = request
        .headers()
        .get(HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| is_safe(v))
        .map(str::to_owned);

    let id = incoming.unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

    if let Ok(value) = HeaderValue::from_str(&id) {
        request.headers_mut().insert(HEADER, value);
    }

    let mut response = REQUEST_ID.scope(id.clone(), next.run(request)).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(HEADER, value);
    }
    response
}

/// 入站 ID 会被写进日志和响应头，必须限制字符集与长度。
///
/// 不这么做的话，上游可以往日志里注入换行来伪造日志行。
fn is_safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::is_safe;

    #[test]
    fn accepts_ordinary_trace_ids() {
        assert!(is_safe("0199a1f0-9c3a-7c11-9f6e-2b1c7a0d4e55"));
        assert!(is_safe("trace_abc.123:1"));
    }

    #[test]
    fn rejects_log_injection_and_oversized_ids() {
        assert!(!is_safe(""));
        assert!(!is_safe("a\nINFO fake log line"));
        assert!(!is_safe("a\r\nb"));
        assert!(!is_safe("<script>"));
        assert!(!is_safe(&"x".repeat(129)));
    }
}
