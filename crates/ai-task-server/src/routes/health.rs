//! 健康检查。
//!
//! `healthz` 与 `readyz` 是两件事，混用会让滚动发布出问题：
//! - `healthz` 只回答「进程还活着吗」。它挂了才该重启容器。
//! - `readyz` 回答「现在能接流量吗」。依赖不可用时它该失败，但**不该**触发重启
//!   ——重启一个数据库连不上的进程只会让恢复更慢。

use std::time::Instant;

use ai_task_proto::{Health, HealthCheck, HealthStatus};
use axum::extract::State;
use axum::http::StatusCode;

use crate::extract::Json;
use crate::state::AppState;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `GET /api/v1/healthz` —— 存活探针。不碰任何依赖。
pub async fn healthz() -> Json<Health> {
    Json(Health {
        status: HealthStatus::Ok,
        version: VERSION.to_string(),
        checks: Vec::new(),
    })
}

/// `GET /api/v1/readyz` —— 就绪探针。探测下游，失败返回 503。
pub async fn readyz(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let started = Instant::now();
    let db = match state.store.ping().await {
        Ok(()) => HealthCheck {
            name: "postgres".into(),
            status: HealthStatus::Ok,
            detail: None,
            latency_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        },
        Err(err) => {
            tracing::warn!(error = %err, "readyz: postgres 探测失败");
            HealthCheck {
                name: "postgres".into(),
                status: HealthStatus::Down,
                // 只给类别，不回传连接串或主机名
                detail: Some("连接或查询失败".into()),
                latency_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            }
        }
    };

    let status = if db.status == HealthStatus::Ok {
        HealthStatus::Ok
    } else {
        HealthStatus::Down
    };
    let code = if status == HealthStatus::Ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        code,
        Json(Health {
            status,
            version: VERSION.to_string(),
            checks: vec![db],
        }),
    )
}
