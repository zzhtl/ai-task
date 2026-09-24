//! 请求体、查询串、路径参数的提取器。
//!
//! 用法和 axum 自带的同名提取器一样，区别只在**失败时**：axum 的拒绝是一段纯文本，
//! 前端的 `parseError` 解析不出 `ApiError`，只能显示 `http_422 Unprocessable Entity`
//! ——用户知道"保存失败了"，但不知道是哪个字段。这里把拒绝包成和其它错误一样的信封，
//! JSON 里某个字段不对时带上它的路径（`spec.nodes[0].config`）。
//!
//! 状态码不变：语法错 400、字段不对 422、不是 JSON 415、太大 413。

use axum::body::Bytes;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::AppError;

/// JSON 请求体 / 响应体。作为响应时和 `axum::Json` 完全一样。
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json(request.headers()) {
            return Err(AppError::Rejected {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                message: "请求体要是 JSON（Content-Type: application/json）".into(),
            });
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| AppError::Rejected {
                status: rejection.status(),
                message: rejection.body_text(),
            })?;
        parse_json(&bytes).map(Json)
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// 把 JSON 字节解析成 `T`。字段层面的错误是 422 并带路径，语法错误是 400。
///
/// 路径来自 serde_path_to_error：`deny_unknown_fields` 报的"未知字段"、类型不对、
/// 缺字段，都能定位到具体的那一层，而不是一句"报文解析失败"。
pub fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AppError> {
    let deserializer = &mut serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(deserializer).map_err(|err| {
        let path = err.path().to_string();
        let inner = err.into_inner();
        match inner.classify() {
            serde_json::error::Category::Data => {
                AppError::Validation(vec![ai_task_proto::FieldError {
                    // 根上的错误（缺字段、未知字段）路径是 "."，给个人能认的名字
                    field: if path == "." { "body".into() } else { path },
                    code: "invalid".into(),
                    message: inner.to_string(),
                }])
            }
            _ => AppError::BadRequest(format!("请求体不是合法的 JSON：{inner}")),
        }
    })
}

/// 查询串。
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(value)| Query(value))
            // 未声明的查询参数被拒绝而不是忽略（拼错一个名字不能变成"返回全部"）。
            // 这里只是把那句话装进信封，让界面显示得出来
            .map_err(|rejection| AppError::BadRequest(rejection.body_text()))
    }
}

/// 路径参数。
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(value)| Path(value))
            .map_err(|rejection| {
                if rejection.status().is_client_error() {
                    AppError::BadRequest(rejection.body_text())
                } else {
                    // 路由表和 handler 签名对不上：这是服务端的 bug，不是请求的错
                    AppError::Internal(anyhow::anyhow!(rejection.body_text()))
                }
            })
    }
}

/// `application/json` 或 `application/*+json`。
fn is_json(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    essence == "application/json"
        || (essence.starts_with("application/") && essence.ends_with("+json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Inner {
        #[allow(dead_code)]
        port: u16,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Outer {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        hosts: Vec<Inner>,
    }

    fn field_of(err: AppError) -> (String, String) {
        match err {
            AppError::Validation(details) => (details[0].field.clone(), details[0].message.clone()),
            other => panic!("应该是 422，实际是 {other:?}"),
        }
    }

    #[test]
    fn a_bad_nested_field_is_422_with_its_path() {
        let err = parse_json::<Outer>(br#"{"name":"x","hosts":[{"port":22},{"port":"ssh"}]}"#)
            .expect_err("port 类型不对");
        let (field, message) = field_of(err);
        assert_eq!(field, "hosts[1].port");
        assert!(message.contains("invalid type"), "{message}");
    }

    #[test]
    fn an_unknown_field_is_reported_under_its_own_name() {
        // deny_unknown_fields 报的未知字段，路径就是那个拼错的名字
        let err =
            parse_json::<Outer>(br#"{"name":"x","hosts":[],"hsots":[]}"#).expect_err("拼错了");
        let (field, message) = field_of(err);
        assert_eq!(field, "hsots");
        assert!(message.contains("unknown field"), "{message}");
    }

    #[test]
    fn a_missing_top_level_field_is_reported_on_the_body() {
        // 缺字段时路径停在根上（"."），换成人能认的 "body"，消息里带着缺的是哪个
        let err = parse_json::<Outer>(br#"{"hosts":[]}"#).expect_err("缺 name");
        let (field, message) = field_of(err);
        assert_eq!(field, "body");
        assert!(message.contains("name"), "{message}");
    }

    #[test]
    fn broken_json_is_400_not_422() {
        match parse_json::<Outer>(br#"{"name":"#) {
            Err(AppError::BadRequest(message)) => assert!(message.contains("JSON"), "{message}"),
            other => panic!("语法错误应该是 400：{other:?}"),
        }
    }

    #[test]
    fn json_content_types() {
        let mut headers = HeaderMap::new();
        assert!(!is_json(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            "application/json; charset=utf-8".parse().expect("合法的头"),
        );
        assert!(is_json(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            "application/merge-patch+json".parse().expect("合法的头"),
        );
        assert!(is_json(&headers));
        headers.insert(
            header::CONTENT_TYPE,
            "text/plain".parse().expect("合法的头"),
        );
        assert!(!is_json(&headers));
    }
}
