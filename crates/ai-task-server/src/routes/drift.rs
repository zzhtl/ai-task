//! 漂移比较。
//!
//! 回答的是同一个问题：**输入没变、输出变了吗。**
//! 输入变了而输出跟着变，那是改动生效了，不是漂移。

use ai_task_core::drift;
use ai_task_proto::RunId;
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareQuery {
    /// 基线 run。不给就用这个 run 自己的 `compare_to`，
    /// 再不行就取同任务上一个**同指纹**的成功 run。
    #[serde(default)]
    pub baseline: Option<RunId>,
}

#[derive(Debug, Serialize)]
pub struct DriftReport {
    pub run_id: String,
    /// 实际用作基线的 run。`None` 表示找不到可比的基线。
    pub baseline_run_id: Option<String>,
    /// 为什么没有基线。有基线时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_baseline_reason: Option<String>,
    /// 输入条件相同吗。
    pub same_fingerprint: bool,
    /// 输出摘要相同吗。
    pub same_digest: bool,
    /// **该不该告警**：输入没变、输出却变了。
    pub alarming: bool,
    pub changes: Vec<Change>,
    /// 影子执行的 run。它的副作用工具只记录意图不执行。
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub struct Change {
    pub path: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}

/// `GET /api/v1/runs/{id}/drift`
pub async fn compare(
    State(state): State<AppState>,
    Path(id): Path<uuid::Uuid>,
    Query(query): Query<CompareQuery>,
) -> Result<Json<DriftReport>, AppError> {
    let run_id = RunId(id);
    let run = state.store.get_run(state.workspace_id, run_id).await?;

    // 基线的三级回退，从最明确到最省事
    let baseline = match query.baseline.or(run.compare_to) {
        Some(explicit) => Some(state.store.get_run(state.workspace_id, explicit).await?),
        None => {
            state
                .store
                .previous_comparable_run(
                    state.workspace_id,
                    run.task_id,
                    run_id,
                    run.fingerprint.as_deref(),
                )
                .await?
        }
    };

    let Some(baseline) = baseline else {
        return Ok(Json(DriftReport {
            run_id: run_id.to_string(),
            baseline_run_id: None,
            no_baseline_reason: Some(
                "同任务下没有更早的同指纹成功 run 可作基线。第一次跑、\
                 或者刚改过任务定义时都是这样。"
                    .to_owned(),
            ),
            same_fingerprint: false,
            same_digest: false,
            alarming: false,
            changes: Vec::new(),
            dry_run: run.dry_run,
        }));
    };

    let drift = drift::compare(
        baseline.fingerprint.as_deref(),
        baseline.output.as_ref(),
        run.fingerprint.as_deref(),
        run.output.as_ref(),
    );

    Ok(Json(DriftReport {
        run_id: run_id.to_string(),
        baseline_run_id: Some(baseline.id.to_string()),
        no_baseline_reason: None,
        same_fingerprint: drift.same_fingerprint,
        same_digest: drift.same_digest,
        alarming: drift.is_alarming(),
        changes: drift
            .changes
            .into_iter()
            .map(|c| Change {
                path: c.path,
                before: c.before,
                after: c.after,
            })
            .collect(),
        dry_run: run.dry_run,
    }))
}
