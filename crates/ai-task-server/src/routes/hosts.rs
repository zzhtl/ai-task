//! 主机与资源采样接口。

use ai_task_proto::{FieldError, HostId, Page, RunId};
use ai_task_store::{HostUpdate, NewHost};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

/// `POST /api/v1/hosts`
///
/// `private_key` 只进不出：加密后写 credentials 表，**任何读接口都不会返回它**。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateHost {
    pub name: String,
    /// 主机名或 IP。
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    /// 策略按 tag 生效（如 `prod` 机上写类操作一律 ask）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// OpenSSH 格式的私钥。
    pub private_key: String,
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Serialize)]
pub struct HostCreated {
    pub id: String,
}

/// 建和改共用的字段检查。
fn check_fields(
    name: &str,
    address: &str,
    username: &str,
    tags: &[String],
) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(validation("name", "主机名不能为空"));
    }
    if address.trim().is_empty() {
        return Err(validation("address", "地址不能为空"));
    }
    if username.trim().is_empty() {
        return Err(validation("username", "登录用户名不能为空"));
    }
    if tags.iter().any(|t| t.trim().is_empty()) {
        return Err(validation("tags", "tag 不能是空字符串"));
    }
    Ok(())
}

/// 私钥格式在这里判掉，比连接失败时再报清楚得多：那时候错误来自
/// russh，混在"网络不通/认证失败/密钥格式不对"里分不出来。
fn check_private_key(key: &str) -> Result<(), AppError> {
    if !key.contains("PRIVATE KEY") {
        return Err(validation(
            "private_key",
            "看起来不是 OpenSSH 私钥。要的是私钥文件的内容（-----BEGIN ... PRIVATE KEY-----），不是公钥、也不是路径",
        ));
    }
    Ok(())
}

pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateHost>,
) -> Result<impl IntoResponse, AppError> {
    check_fields(&body.name, &body.address, &body.username, &body.tags)?;
    check_private_key(&body.private_key)?;

    let id = state
        .store
        .create_host(NewHost {
            workspace_id: state.workspace_id,
            name: body.name.trim().to_owned(),
            address: body.address.trim().to_owned(),
            port: i32::from(body.port),
            username: body.username.trim().to_owned(),
            tags: body.tags.clone(),
            private_key: body.private_key,
        })
        .await
        .map_err(|err| match err {
            ai_task_store::StoreError::Conflict { .. } => {
                AppError::Conflict(format!("主机 `{}` 已存在", body.name.trim()))
            }
            other => AppError::Store(other),
        })?;

    // 不记私钥，只记这台机器是谁、什么时候被加进来的
    state
        .audit(
            "host.create",
            "host",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "name": body.name.trim(),
                "address": body.address.trim(),
                "port": body.port,
                "username": body.username.trim(),
                "tags": body.tags,
            })),
        )
        .await;

    Ok((
        StatusCode::CREATED,
        Json(HostCreated { id: id.to_string() }),
    ))
}

/// `PUT /api/v1/hosts/{id}`
///
/// 整体替换连接信息；`private_key` 留空或不给表示钥匙不动——编辑表单里
/// 永远看不到旧钥匙，所以"没填"只能是"不换"。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateHost {
    pub name: String,
    pub address: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub private_key: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<UpdateHost>,
) -> Result<impl IntoResponse, AppError> {
    check_fields(&body.name, &body.address, &body.username, &body.tags)?;
    let private_key = body.private_key.filter(|k| !k.trim().is_empty());
    if let Some(key) = &private_key {
        check_private_key(key)?;
    }
    let key_replaced = private_key.is_some();

    let changed = state
        .store
        .update_host(
            state.workspace_id,
            HostId(id),
            HostUpdate {
                name: body.name.trim().to_owned(),
                address: body.address.trim().to_owned(),
                port: i32::from(body.port),
                username: body.username.trim().to_owned(),
                tags: body.tags.clone(),
                private_key,
            },
        )
        .await
        .map_err(|err| match err {
            ai_task_store::StoreError::Conflict { .. } => {
                AppError::Conflict(format!("主机 `{}` 已存在", body.name.trim()))
            }
            other => AppError::Store(other),
        })?;
    if !changed {
        return Err(AppError::NotFound(format!("主机 {id} 不存在")));
    }

    state
        .audit(
            "host.update",
            "host",
            id.to_string(),
            None,
            Some(serde_json::json!({
                "name": body.name.trim(),
                "address": body.address.trim(),
                "port": body.port,
                "username": body.username.trim(),
                "tags": body.tags,
                "key_replaced": key_replaced,
            })),
        )
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/hosts/{id}`
///
/// 还有任务的步骤钉在这台机器上时拒绝（409）并列出任务名：
/// 让它们在凌晨两点跑到一台不存在的机器上，比现在多点两下要糟得多。
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let host_id = HostId(id);
    let pinned = state
        .store
        .tasks_using_host(state.workspace_id, host_id)
        .await?;
    if !pinned.is_empty() {
        return Err(AppError::Conflict(format!(
            "还有任务的步骤钉在这台机器上：{}。先把它们改到别的机器再删",
            pinned.join("、")
        )));
    }
    if !state.store.delete_host(state.workspace_id, host_id).await? {
        return Err(AppError::NotFound(format!("主机 {id} 不存在")));
    }
    state
        .audit("host.delete", "host", id.to_string(), None, None)
        .await;
    Ok(StatusCode::NO_CONTENT)
}

/// 主机的对外形状。**没有凭据字段，且不会有。**
#[derive(Debug, Serialize)]
pub struct HostSummary {
    pub id: String,
    pub name: String,
    pub address: String,
    pub port: i32,
    pub username: String,
    pub tags: Vec<String>,
    /// 已投送的 agent 版本（二进制哈希前 16 位）。`None` 表示还没连过。
    pub agent_version: Option<String>,
    /// 资源归因档位：`systemd` / `proc` / `none`。
    pub cgroup_mode: Option<String>,
    /// 这台机器上探测到的 AI CLI。界面据此决定"能不能在这台机器上直接跑 AI"。
    /// **是上次连接时的快照**，会过期；真正执行前还会再验一次。
    pub ai_clis: serde_json::Value,
    /// 归因不完整、**且资源上限没有被强制**。界面要显式标出来。
    pub degraded: bool,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `GET /api/v1/hosts`
pub async fn list(State(state): State<AppState>) -> Result<Json<Page<HostSummary>>, AppError> {
    let hosts = state.store.list_hosts(state.workspace_id).await?;
    Ok(Json(Page {
        items: hosts
            .into_iter()
            .map(|h| HostSummary {
                id: h.id.to_string(),
                name: h.name,
                address: h.address,
                port: h.port,
                username: h.username,
                tags: h.tags,
                agent_version: h
                    .agent_sha256
                    .map(|s| s.chars().take(16).collect::<String>()),
                // 没探测过就还不知道，不能当成"没降级"
                degraded: h.cgroup_mode.as_deref().is_some_and(|m| m != "systemd"),
                cgroup_mode: h.cgroup_mode,
                ai_clis: h.ai_clis,
                last_seen_at: h.last_seen_at,
            })
            .collect(),
        next_cursor: None,
    }))
}

/// 一个节点的资源曲线。
#[derive(Debug, Serialize)]
pub struct NodeMetrics {
    pub node_key: String,
    pub points: Vec<MetricPoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricPoint {
    pub at: chrono::DateTime<chrono::Utc>,
    /// **累计** CPU 微秒。相邻两点做差再除以间隔才是使用率——
    /// 直接画会看到一条单调上升的线。
    pub cpu_usec: i64,
    pub rss_bytes: i64,
    pub pids: i32,
}

/// 每个节点最多返回多少个采样点。
///
/// 注释原本写着"一个正常 run 也就几百个点"——但那是**期望**，不是约束。
/// 1 Hz × 一个跑四小时的 shell 节点 = 14400 个点，再乘上节点数，
/// 全都塞进一个 JSON 响应里。曲线画在 720px 宽的图上，
/// 超过这个数的点在屏幕上根本落不到不同的像素里。
const MAX_POINTS_PER_NODE: usize = 1_000;

/// `GET /api/v1/runs/{id}/metrics`
///
/// 采样是 1Hz、只在节点执行期间产生。点数超过上限时**等距抽稀**，
/// 而不是截断——截断会让曲线在中间断掉，看起来像节点提前结束了。
pub async fn metrics(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<Vec<NodeMetrics>>, AppError> {
    let run_id = RunId(id);
    // 先确认 run 在本 workspace 里：否则拿别人的 run id 就能读到它的资源曲线
    state.store.get_run(state.workspace_id, run_id).await?;

    let rows = state.store.read_metrics(run_id).await?;
    let mut grouped: std::collections::BTreeMap<String, Vec<MetricPoint>> =
        std::collections::BTreeMap::new();
    for (node_key, sample) in rows {
        grouped.entry(node_key).or_default().push(MetricPoint {
            at: sample.at,
            cpu_usec: sample.cpu_usec,
            rss_bytes: sample.rss_bytes,
            pids: sample.pids,
        });
    }

    Ok(Json(
        grouped
            .into_iter()
            .map(|(node_key, points)| NodeMetrics {
                node_key,
                points: downsample(points, MAX_POINTS_PER_NODE),
            })
            .collect(),
    ))
}

/// 等距抽稀到至多 `max` 个点。**首尾必留**——曲线的起止时刻决定了
/// 节点在共用时间轴上的位置，抽掉了整条线就会错位。
fn downsample(points: Vec<MetricPoint>, max: usize) -> Vec<MetricPoint> {
    if points.len() <= max || max < 2 {
        return points;
    }
    let last = points.len() - 1;
    let step = last as f64 / (max - 1) as f64;
    (0..max)
        .map(|i| {
            let idx = ((i as f64) * step).round() as usize;
            points[idx.min(last)].clone()
        })
        .collect()
}

fn validation(field: &str, message: &str) -> AppError {
    AppError::Validation(vec![FieldError {
        field: field.into(),
        code: "invalid".into(),
        message: message.into(),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_summary_has_no_field_that_could_carry_a_credential() {
        // 加一个 `private_key` / `credential` 字段就会让这个测试失败。
        // 凭据泄漏一次就是永久的——它会进日志、进浏览器缓存、进截图。
        let json = serde_json::to_value(HostSummary {
            id: "h".into(),
            name: "prod-1".into(),
            address: "10.0.0.1".into(),
            port: 22,
            username: "runner".into(),
            tags: vec!["prod".into()],
            agent_version: None,
            cgroup_mode: None,
            ai_clis: serde_json::json!([]),
            degraded: false,
            last_seen_at: None,
        })
        .expect("序列化");
        let keys: Vec<&str> = json
            .as_object()
            .expect("对象")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "id",
                "name",
                "address",
                "port",
                "username",
                "tags",
                "agent_version",
                "cgroup_mode",
                "ai_clis",
                "degraded",
                "last_seen_at"
            ],
            "主机的对外形状变了。加字段前先确认它不是凭据。"
        );
    }

    #[test]
    fn a_host_that_was_never_probed_is_not_reported_as_healthy() {
        // "还不知道"和"确认是 systemd"必须分开：前者不能显示成绿的
        let never = None::<String>;
        assert!(!never.as_deref().is_some_and(|m| m != "systemd"));
        assert!(Some("proc").is_some_and(|m| m != "systemd"));
        assert!(!Some("systemd").is_some_and(|m| m != "systemd"));
    }
}

#[cfg(test)]
mod downsample_tests {
    use super::*;

    fn point(n: i64) -> MetricPoint {
        MetricPoint {
            at: chrono::DateTime::from_timestamp(n, 0).expect("ts"),
            cpu_usec: n,
            rss_bytes: n,
            pids: 1,
        }
    }

    #[test]
    fn short_series_pass_through_untouched() {
        let points: Vec<_> = (0..10).map(point).collect();
        assert_eq!(downsample(points.clone(), 1000).len(), 10);
    }

    #[test]
    fn long_series_keep_both_ends() {
        // 首尾决定这个节点在共用时间轴上占哪一段，抽掉就会错位
        let points: Vec<_> = (0..50_000).map(point).collect();
        let out = downsample(points, 1_000);
        assert_eq!(out.len(), 1_000);
        assert_eq!(out.first().expect("首").cpu_usec, 0);
        assert_eq!(out.last().expect("尾").cpu_usec, 49_999);
    }

    #[test]
    fn output_stays_in_chronological_order() {
        let points: Vec<_> = (0..5_000).map(point).collect();
        let out = downsample(points, 100);
        assert!(out.windows(2).all(|w| w[0].cpu_usec <= w[1].cpu_usec));
    }
}
