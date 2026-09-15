//! 内嵌前端资源的服务。
//!
//! 缓存策略跟着 SvelteKit 的产物布局走：`_app/immutable/` 下的文件名带内容哈希，
//! 可以长期强缓存；`index.html` 必须每次校验，否则改版后用户一直拿到旧壳。

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode, Uri, header};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "$CARGO_MANIFEST_DIR/dist"]
pub struct Assets;

/// SvelteKit（adapter-static）把带哈希的产物放在这个前缀下。
const IMMUTABLE_PREFIX: &str = "_app/immutable/";

pub async fn serve(uri: Uri, headers: HeaderMap) -> Response<Body> {
    let path = uri.path().trim_start_matches('/');
    let key = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Assets::get(key) {
        return build_response(key, file, &headers, precompressed(key, &headers));
    }

    // 带扩展名的资源没命中就是真的 404。一律回退 index.html 并返回 200 会让
    // 缺失的 .js/.css 看起来"加载成功"，把构建产物不完整的问题掩盖掉。
    if has_extension(key) {
        return not_found();
    }

    // 无扩展名的路径视为前端路由，交回 SPA 入口
    match Assets::get("index.html") {
        Some(file) => build_response("index.html", file, &headers, None),
        None => not_found(),
    }
}

fn has_extension(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|seg| seg.contains('.'))
}

fn not_found() -> Response<Body> {
    let mut response = Response::new(Body::from("404 Not Found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

/// 客户端能收哪种编码，我们又恰好有对应的侧车。
///
/// `precompress: true` 让构建期就把 `.br` / `.gz` 生成好。这些是内容哈希过的
/// 不可变资源，每个请求现压一遍是纯浪费 CPU——而且压的还是同样的字节。
/// 命中侧车时直接吐，并打上 `content-encoding`，压缩层看到就不会再压一次。
fn precompressed(
    path: &str,
    headers: &HeaderMap,
) -> Option<(rust_embed::EmbeddedFile, &'static str)> {
    let accept = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    // brotli 更小，优先
    for (suffix, encoding) in [(".br", "br"), (".gz", "gzip")] {
        if !accept.contains(encoding) {
            continue;
        }
        if let Some(file) = Assets::get(&format!("{path}{suffix}")) {
            return Some((file, encoding));
        }
    }
    None
}

fn build_response(
    path: &str,
    file: rust_embed::EmbeddedFile,
    headers: &HeaderMap,
    precompressed: Option<(rust_embed::EmbeddedFile, &'static str)>,
) -> Response<Body> {
    let etag = format!("\"{}\"", hex(&file.metadata.sha256_hash()));

    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|part| part.trim() == etag));

    let mut response = if fresh {
        let mut r = Response::new(Body::empty());
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        r
    } else {
        // ETag 始终来自**未压缩**的那份：同一份内容不该因为客户端支持的编码不同
        // 而有两个不同的 ETag，否则换个浏览器就得重下一遍。
        let body = precompressed
            .as_ref()
            .map_or(file.data, |(sidecar, _)| sidecar.data.clone());
        // `into_owned()` 会把整个文件复制进一个新 Vec——每一个请求都复制一遍，
        // 而这些字节是 'static 的、内容哈希过的、永远不变的。
        // release 下 rust-embed 给的是 Cow::Borrowed，借用它就是零拷贝；
        // dev 下（没开 debug-embed）是 Owned，直接拿走，行为不变。
        Response::new(match body {
            std::borrow::Cow::Borrowed(bytes) => Body::from(Bytes::from_static(bytes)),
            std::borrow::Cow::Owned(bytes) => Body::from(bytes),
        })
    };

    let out = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        out.insert(header::ETAG, value);
    }
    if fresh {
        return response;
    }

    if let Some((_, encoding)) = precompressed {
        out.insert(header::CONTENT_ENCODING, HeaderValue::from_static(encoding));
        // 同一个 URL 在不同 accept-encoding 下内容不同，缓存必须按它分桶
        out.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
    }

    let mime = mime_guess::from_path(path).first_or_octet_stream();
    out.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime.as_ref())
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    out.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if path.starts_with(IMMUTABLE_PREFIX) {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    response
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_asset_paths() {
        assert!(has_extension("_app/immutable/chunks/abc.js"));
        assert!(has_extension("favicon.svg"));
        assert!(!has_extension("runs"));
        assert!(!has_extension("runs/0199a1f0-9c3a-7c11-9f6e-2b1c7a0d4e55"));
    }

    #[test]
    fn frontend_bundle_is_actually_embedded() {
        // 构建产物没进二进制的话，整个服务只会返回 404，而且很难看出来
        assert!(
            Assets::get("index.html").is_some(),
            "dist/index.html 没被内嵌"
        );
        assert!(
            Assets::iter().any(|p| p.starts_with(IMMUTABLE_PREFIX)),
            "没有内嵌任何带哈希的产物，缓存前缀 {IMMUTABLE_PREFIX} 可能已经变了"
        );
    }

    #[tokio::test]
    async fn unknown_route_falls_back_to_spa_but_missing_asset_is_a_real_404() {
        let spa = serve("/runs/abc".parse().expect("uri"), HeaderMap::new()).await;
        assert_eq!(spa.status(), StatusCode::OK);

        let missing = serve(
            "/_app/immutable/nope.js".parse().expect("uri"),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(
            missing.status(),
            StatusCode::NOT_FOUND,
            "缺失的静态资源必须真 404，不能被 SPA 回退掩盖"
        );
    }

    #[tokio::test]
    async fn hashed_assets_are_immutable_and_index_is_revalidated() {
        let index = serve("/".parse().expect("uri"), HeaderMap::new()).await;
        assert_eq!(
            index
                .headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.as_bytes()),
            Some(b"no-cache".as_slice())
        );

        let hashed = Assets::iter()
            .find(|p| p.starts_with(IMMUTABLE_PREFIX))
            .expect("应当有带哈希的产物");
        let response = serve(format!("/{hashed}").parse().expect("uri"), HeaderMap::new()).await;
        let cache = response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.as_bytes());
        assert_eq!(
            cache,
            Some(b"public, max-age=31536000, immutable".as_slice())
        );
    }

    #[tokio::test]
    async fn matching_etag_returns_304_without_a_body() {
        let first = serve("/".parse().expect("uri"), HeaderMap::new()).await;
        let etag = first
            .headers()
            .get(header::ETAG)
            .expect("应当带 ETag")
            .clone();

        let mut headers = HeaderMap::new();
        headers.insert(header::IF_NONE_MATCH, etag);
        let second = serve("/".parse().expect("uri"), headers).await;
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }
}
