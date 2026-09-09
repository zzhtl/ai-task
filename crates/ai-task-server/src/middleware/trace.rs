//! 请求 span。

use axum::extract::Request;
use tower_http::trace::MakeSpan;
use tracing::Span;

/// 每个请求一个 span，带上 request_id，这样 handler 内部的日志自动继承它。
#[derive(Clone, Copy, Debug)]
pub struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let request_id = request
            .headers()
            .get(super::request_id::HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");

        tracing::info_span!(
            "http",
            method = %request.method(),
            path = %request.uri().path(),
            request_id = %request_id,
        )
    }
}
