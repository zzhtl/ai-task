//! HTTP 错误契约。
//!
//! 状态码只表达**问题的类别**，具体原因由 `code` 承载。完整的状态码策略
//! （400 与 422 的分工、403 与 404 的取舍、503 不用 500 等）见
//! `docs/adr/0002-api-contract.md`；这里只放当前真正会被构造的变体，
//! 后续里程碑用到哪个就连同它的测试一起加回来。

use ai_task_proto::ApiError;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::middleware::request_id;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 资源不存在。**跨租户资源也走这里**：返回 403 等于确认了它的存在。
    #[error("{0}")]
    NotFound(String),

    /// 未认证。内部接口的令牌不匹配也走这里。
    #[error("{0}")]
    Unauthorized(&'static str),

    /// 认了出来，但这一档不够。
    ///
    /// 只用于**同租户内的权限不足**；跨租户资源一律走 `NotFound`，
    /// 因为 403 等于向探测者确认了那个 id 存在。
    #[error("{0}")]
    Forbidden(String),

    /// 报文解析不了（游标格式错、JSON 坏掉）。
    #[error("{0}")]
    BadRequest(String),

    /// 报文合法但语义非法。字段级错误**一次全部返回**，不是只报第一个。
    #[error("请求校验未通过")]
    Validation(Vec<ai_task_proto::FieldError>),

    /// 状态冲突：重名、非法状态迁移。
    #[error("{0}")]
    Conflict(String),

    /// 在进 handler 之前就被拒的请求：不是 JSON（415）、太大（413）之类。
    /// 状态码沿用原来的，只是换成统一的信封。
    #[error("{message}")]
    Rejected { status: StatusCode, message: String },

    /// `If-Match` 的前置条件不成立。
    ///
    /// 和 409 分开：409 是"这个操作现在做不了"，412 是"你以为的状态不是现在的状态"
    /// ——客户端的处理方式不同，前者可能要换个做法，后者只需重读再重试。
    #[error("{0}")]
    PreconditionFailed(String),

    #[error(transparent)]
    Store(#[from] ai_task_store::StoreError),

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    /// 状态码与稳定的 `code`。
    ///
    /// **`code` 是契约**：新增是兼容变更，改变已有 code 的触发条件是破坏性变更。
    fn classify(&self) -> (StatusCode, &'static str) {
        match self {
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::PreconditionFailed(_) => (StatusCode::PRECONDITION_FAILED, "precondition_failed"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            // 全项目只用 422 表示「读懂了但你说的不对」，不与 400 混用
            Self::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_failed"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Rejected { status, .. } => (
                *status,
                match *status {
                    StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
                    StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
                    _ => "bad_request",
                },
            ),
            Self::Store(_) | Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code) = self.classify();
        let request_id = request_id::current();

        if status.is_server_error() {
            tracing::error!(error = %self, %request_id, "请求失败");
        } else {
            tracing::warn!(error = %self, %request_id, %code, "请求被拒绝");
        }

        let (message, details) = match &self {
            // 内部错误的细节只进日志。响应体里只留 request_id，让用户能报给运维，
            // 而不是把栈、SQL、内部主机名泄出去。
            Self::Store(_) | Self::Internal(_) => (
                format!("服务内部错误。请把 request_id {request_id} 提供给运维。"),
                Vec::new(),
            ),
            Self::Validation(details) => (self.to_string(), details.clone()),
            _ => (self.to_string(), Vec::new()),
        };

        (
            status,
            Json(ApiError {
                code: code.to_string(),
                message,
                request_id,
                details,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn internal_errors_do_not_leak_details_to_the_client() {
        let err = AppError::Internal(anyhow::anyhow!(
            "connection to host db-prod-3.internal failed: password authentication failed"
        ));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let text = String::from_utf8_lossy(&body);
        assert!(!text.contains("db-prod-3"), "内部主机名泄漏了：{text}");
        assert!(!text.contains("password"), "内部错误细节泄漏了：{text}");
        assert!(text.contains("internal"));
    }

    #[tokio::test]
    async fn forbidden_uses_the_same_envelope_as_every_other_error() {
        // RBAC 曾经手拼 403 的 JSON，少了 request_id 和 details——于是全站唯一一个
        // 客户端解析不出 ApiError 的响应，恰好落在最需要把 request_id 报给运维的地方。
        let response =
            AppError::Forbidden("需要 admin 及以上，你是 operator".into()).into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let parsed: ApiError = serde_json::from_slice(&body).expect("403 必须能解析成 ApiError");
        assert_eq!(parsed.code, "forbidden");
        assert!(parsed.message.contains("admin"));
        // request_id 字段必须在，哪怕测试里不在请求上下文中拿到的是占位值
        assert!(parsed.details.is_empty());
    }

    #[tokio::test]
    async fn validation_is_422_and_reports_every_field_at_once() {
        use ai_task_proto::FieldError;
        let err = AppError::Validation(vec![
            FieldError {
                field: "spec".into(),
                code: "invalid_dag".into(),
                message: "存在环".into(),
            },
            FieldError {
                field: "spec".into(),
                code: "invalid_dag".into(),
                message: "节点 key 重复".into(),
            },
        ]);
        // 400 与 422 的分工全项目一致：能解析但语义非法一律 422
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let api: ApiError = serde_json::from_slice(&body).expect("解析");
        assert_eq!(api.code, "validation_failed");
        assert_eq!(api.details.len(), 2, "校验错误要一次报全，不是只报第一个");
    }

    #[tokio::test]
    async fn not_found_message_reaches_the_client() {
        let response = AppError::NotFound("接口 /api/v1/nope 不存在".into()).into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("读 body");
        let err: ApiError = serde_json::from_slice(&body).expect("解析");
        assert_eq!(err.code, "not_found");
        assert!(err.message.contains("/api/v1/nope"));
    }
}
