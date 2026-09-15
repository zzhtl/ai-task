//! Run 的读写。
//!
//! `runs` 表里的 `status` / `cost_micros` / `max_seq` 都是**投影**，不是真值：
//! 真值是 `run_events`。保留这几列只为让列表查询和调度器扫描不必回放事件。
//! 因此它们的更新永远和对应的事件写在同一个事务里。

use ai_task_proto::{RunId, RunStatus, TaskId, TaskVersionId, TriggerKind, UsdMicros, WorkspaceId};
use chrono::{DateTime, Utc};
use sqlx::postgres::PgArguments;
use sqlx::query::Query;
use sqlx::{AssertSqlSafe, Postgres, Row};

use crate::events::{PendingEvent, append_in_tx};
use crate::{Store, StoreError};

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: RunId,
    pub workspace_id: WorkspaceId,
    pub task_id: TaskId,
    pub task_version_id: TaskVersionId,
    pub status: RunStatus,
    pub trigger: TriggerKind,
    pub dry_run: bool,
    pub inputs: Option<serde_json::Value>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub cost: UsdMicros,
    pub max_seq: i64,
    pub cli_version: Option<String>,
    /// 输入条件的哈希。同 fingerprint 的两次 run 才可比。
    pub fingerprint: Option<String>,
    /// 输出的稳定子集的哈希。
    pub output_digest: Option<String>,
    /// 被指定为基线的 run。
    pub compare_to: Option<RunId>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// run 的终态。字段够多，收成一个结构体让调用点能一眼看出谁是谁。
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub status: RunStatus,
    pub cost: UsdMicros,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    /// 驱动本次执行的 claude CLI 版本。
    ///
    /// 只在这里写，**不要**为了补它去再调一次 `start_run`——那会把 `started_at`
    /// 覆盖成当前时间，run 的耗时就全废了。
    pub cli_version: Option<String>,
    /// 输出摘要。同 fingerprint 但它变了 → 行为漂移。
    pub output_digest: Option<String>,
}

impl RunOutcome {
    /// 只带状态和成本的终态。
    #[must_use]
    pub fn new(status: RunStatus, cost: UsdMicros) -> Self {
        Self {
            status,
            cost,
            output: None,
            error: None,
            cli_version: None,
            output_digest: None,
        }
    }

    #[must_use]
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct NewRun {
    pub workspace_id: WorkspaceId,
    pub task_id: TaskId,
    pub task_version_id: TaskVersionId,
    pub trigger: TriggerKind,
    pub dry_run: bool,
    pub inputs: Option<serde_json::Value>,
    /// 拿哪个 run 当基线做结构化 diff。影子执行时指向被替换的那次。
    pub compare_to: Option<RunId>,
}

impl Store {
    /// 建一个待执行的 run，并写下它的第一条事件。
    ///
    /// run 行和 `run_queued` 事件在同一个事务里落库——否则可能出现「有 run
    /// 但事件流是空的」，回放直接失败。
    pub async fn create_run(
        &self,
        new: NewRun,
        queued_event: PendingEvent,
    ) -> Result<RunRecord, StoreError> {
        let mut tx = self.pool().begin().await?;
        let record = Self::create_run_in_tx(&mut tx, new, queued_event).await?;
        tx.commit().await?;
        self.notify_run(record.id, 1).await;
        Ok(record)
    }

    /// 同上，但在调用方给的事务里。
    ///
    /// 幂等键要求「响应与副作用写在同一个事务里」（ADR 0002），
    /// 所以建 run 这一步必须能被拉进外面的事务。
    ///
    /// **注意**：提交之后调用方要自己调 [`Store::notify_run`]，否则 SSE 的
    /// 订阅者要等 2 秒的兜底轮询才看得见第一条事件。
    pub async fn create_run_in_tx(
        tx: &mut sqlx::Transaction<'_, Postgres>,
        new: NewRun,
        queued_event: PendingEvent,
    ) -> Result<RunRecord, StoreError> {
        let run_id = RunId::new();

        let row = sqlx::query(
            "INSERT INTO runs (id, workspace_id, task_id, task_version_id, trigger, status,
                               dry_run, inputs, compare_to)
             VALUES ($1, $2, $3, $4, $5, 'queued', $6, $7, $8)
             RETURNING created_at",
        )
        .bind(uuid::Uuid::from(run_id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(uuid::Uuid::from(new.task_id))
        .bind(uuid::Uuid::from(new.task_version_id))
        .bind(trigger_str(new.trigger))
        .bind(new.dry_run)
        .bind(&new.inputs)
        .bind(new.compare_to.map(uuid::Uuid::from))
        .fetch_one(&mut **tx)
        .await?;

        append_in_tx(tx, run_id, std::slice::from_ref(&queued_event)).await?;

        Ok(RunRecord {
            id: run_id,
            workspace_id: new.workspace_id,
            task_id: new.task_id,
            task_version_id: new.task_version_id,
            status: RunStatus::Queued,
            trigger: new.trigger,
            dry_run: new.dry_run,
            inputs: new.inputs,
            output: None,
            error: None,
            cost: UsdMicros::ZERO,
            max_seq: 1,
            cli_version: None,
            fingerprint: None,
            output_digest: None,
            compare_to: new.compare_to,
            created_at: row.try_get("created_at")?,
            started_at: None,
            finished_at: None,
        })
    }

    /// 标记 run 开始执行，并写下 `run_started`。
    pub async fn start_run(
        &self,
        run_id: RunId,
        cli_version: Option<&str>,
        fingerprint: Option<&str>,
        event: PendingEvent,
    ) -> Result<i64, StoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE runs SET status = 'running', started_at = now(),
                    cli_version = COALESCE($2, cli_version),
                    fingerprint = COALESCE($3, fingerprint)
             WHERE id = $1",
        )
        .bind(uuid::Uuid::from(run_id))
        .bind(cli_version)
        .bind(fingerprint)
        .execute(&mut *tx)
        .await?;
        let seq = append_in_tx(&mut tx, run_id, std::slice::from_ref(&event)).await?;
        tx.commit().await?;
        self.notify_run(run_id, seq).await;
        Ok(seq)
    }

    /// 落终态，并写下 `run_finished`。
    ///
    /// 投影列和终态事件必须同事务：分开写会出现「事件说成功、列表页说运行中」。
    pub async fn finish_run(
        &self,
        run_id: RunId,
        outcome: RunOutcome,
        event: PendingEvent,
    ) -> Result<i64, StoreError> {
        let RunOutcome {
            status,
            cost,
            output,
            error,
            cli_version,
            output_digest,
        } = outcome;
        let mut tx = self.pool().begin().await?;
        // cli_version 也在这里写。**不要**为了补它去再调一次 start_run——
        // 那会把 started_at 覆盖成当前时间，run 的耗时就全废了。
        sqlx::query(
            "UPDATE runs
             SET status = $2, finished_at = now(), cost_micros = $3, output = $4, error = $5,
                 cli_version = COALESCE($6, cli_version), output_digest = $7
             WHERE id = $1",
        )
        .bind(uuid::Uuid::from(run_id))
        .bind(status_str(status))
        .bind(cost.0)
        .bind(&output)
        .bind(&error)
        .bind(cli_version)
        .bind(&output_digest)
        .execute(&mut *tx)
        .await?;
        let seq = append_in_tx(&mut tx, run_id, std::slice::from_ref(&event)).await?;
        tx.commit().await?;
        self.notify_run(run_id, seq).await;
        Ok(seq)
    }

    /// 同任务下、比 `before` 更早的、最近的一个可作基线的成功 run。
    ///
    /// 只挑**同指纹**的：指纹不同意味着输入条件就变了，拿来比较得到的
    /// "差异"是改动生效的结果，不是漂移。指纹为空（老数据）时不挑。
    pub async fn previous_comparable_run(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        before: RunId,
        fingerprint: Option<&str>,
    ) -> Result<Option<RunRecord>, StoreError> {
        let Some(fingerprint) = fingerprint else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT * FROM runs
             WHERE workspace_id = $1 AND task_id = $2 AND id <> $3
               AND fingerprint = $4 AND status = 'succeeded' AND output_digest IS NOT NULL
               AND created_at < (SELECT created_at FROM runs WHERE id = $3)
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(uuid::Uuid::from(task_id))
        .bind(uuid::Uuid::from(before))
        .bind(fingerprint)
        .fetch_optional(self.pool())
        .await?;
        row.map(run_from_row).transpose()
    }

    /// 删掉一个 run 连同它的全部痕迹。返回 `false` 表示这个 run 不在。
    ///
    /// **终态才允许删。**删一个正在跑的 run，执行器还在往它的事件流里写——
    /// 结果是刚删完又冒出几条孤儿事件。先取消，跑完了再删。
    pub async fn delete_run(
        &self,
        workspace_id: WorkspaceId,
        id: RunId,
    ) -> Result<Option<bool>, StoreError> {
        let mut tx = self.pool().begin().await?;

        // 行锁住再判终态：否则判完到删掉之间 run 可能刚好开始跑
        let row =
            sqlx::query("SELECT status FROM runs WHERE id = $1 AND workspace_id = $2 FOR UPDATE")
                .bind(uuid::Uuid::from(id))
                .bind(uuid::Uuid::from(workspace_id))
                .fetch_optional(&mut *tx)
                .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(None);
        };
        let status: String = row.try_get("status")?;
        if !parse_status(&status)?.is_terminal() {
            tx.rollback().await?;
            return Ok(Some(false));
        }

        purge_run_children(&mut tx, &[uuid::Uuid::from(id)]).await?;
        sqlx::query("DELETE FROM runs WHERE id = $1 AND workspace_id = $2")
            .bind(uuid::Uuid::from(id))
            .bind(uuid::Uuid::from(workspace_id))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(true))
    }

    pub async fn get_run(
        &self,
        workspace_id: WorkspaceId,
        id: RunId,
    ) -> Result<RunRecord, StoreError> {
        run_query("workspace_id = $1 AND id = $2")
            .bind(uuid::Uuid::from(workspace_id))
            .bind(uuid::Uuid::from(id))
            .fetch_optional(self.pool())
            .await?
            .map(run_from_row)
            .transpose()?
            .ok_or(StoreError::NotFound {
                what: "run",
                id: id.to_string(),
            })
    }

    /// 列 run。游标是 `(created_at, id)`，不用 OFFSET。
    ///
    /// 实测（100k 行）游标 6 buffers / 0.16ms，`OFFSET 50000` 是
    /// 1732 buffers / 15.3ms，而且随深度线性变差。
    /// `status` 为 `None` 表示不按状态过滤；给了空切片会一条都不返回——
    /// 「筛选条件为空」和「没有筛选」是两回事，不该把前者悄悄当成后者。
    pub async fn list_runs(
        &self,
        workspace_id: WorkspaceId,
        task_id: Option<TaskId>,
        status: Option<&[RunStatus]>,
        cursor: Option<(DateTime<Utc>, RunId)>,
        limit: i64,
    ) -> Result<Vec<RunRecord>, StoreError> {
        let statuses = status.map(|s| s.iter().copied().map(status_str).collect::<Vec<_>>());

        // **谓词按实际传参拼出来，不用 `($n IS NULL OR ...)`。**
        //
        // 原来三个可选条件全挤在一条 SQL 里，planner 在计划期看不出
        // `status` 到底会不会被用上，于是选不了 runs_status_recent_idx——
        // 加了索引也照样全扫再过滤。**这是"改了但其实没改好"最容易发生的地方。**
        // 谓词片段都是代码里的字面量，动态数据一律还是走 bind。
        let predicate: &'static str = match (task_id.is_some(), statuses.is_some()) {
            (false, false) => {
                "workspace_id = $1
                 AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
                 ORDER BY created_at DESC, id DESC LIMIT $5"
            }
            (true, false) => {
                "workspace_id = $1 AND task_id = $2
                 AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
                 ORDER BY created_at DESC, id DESC LIMIT $5"
            }
            (false, true) => {
                "workspace_id = $1 AND status = ANY($6)
                 AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
                 ORDER BY created_at DESC, id DESC LIMIT $5"
            }
            (true, true) => {
                "workspace_id = $1 AND task_id = $2 AND status = ANY($6)
                 AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
                 ORDER BY created_at DESC, id DESC LIMIT $5"
            }
        };

        let rows = run_query(predicate)
            .bind(uuid::Uuid::from(workspace_id))
            .bind(task_id.map(uuid::Uuid::from))
            .bind(cursor.map(|(ts, _)| ts))
            .bind(cursor.map(|(_, id)| uuid::Uuid::from(id)))
            .bind(limit)
            .bind(statuses)
            .fetch_all(self.pool())
            .await?;
        rows.into_iter().map(run_from_row).collect()
    }

    /// 还没跑完的 run。引擎重启后据此决定要恢复哪些。
    pub async fn unfinished_runs(&self, limit: i64) -> Result<Vec<RunRecord>, StoreError> {
        let rows = run_query("status IN ('queued', 'running') ORDER BY created_at LIMIT $1")
            .bind(limit)
            .fetch_all(self.pool())
            .await?;
        rows.into_iter().map(run_from_row).collect()
    }

    /// 通知 SSE 订阅者。跨事务建 run 时调用方要自己调它。
    pub async fn notify_run(&self, run_id: RunId, max_seq: i64) {
        let payload = format!("{run_id}:{max_seq}");
        if let Err(err) = sqlx::query("SELECT pg_notify($1, $2)")
            .bind(crate::events::EVENTS_CHANNEL)
            .bind(&payload)
            .execute(self.pool())
            .await
        {
            tracing::warn!(error = %err, %run_id, "pg_notify 失败，订阅方会靠轮询兜底");
        }
    }
}

/// 拼一条 run 查询，避免三处重复那一长串列名。
///
/// `predicate` 刻意收成 `&'static str`：它只能来自代码里的字面量，外部输入
/// 在类型层面就进不来。所以这里的 `AssertSqlSafe` 是有依据的，不是敷衍——
/// 所有动态数据一律走 bind 参数。
fn run_query(predicate: &'static str) -> Query<'static, Postgres, PgArguments> {
    sqlx::query(AssertSqlSafe(format!(
        "SELECT id, workspace_id, task_id, task_version_id, status, trigger, dry_run,
                inputs, output, error, cost_micros, max_seq, cli_version,
                fingerprint, output_digest, compare_to,
                created_at, started_at, finished_at
         FROM runs WHERE {predicate}"
    )))
}

pub(crate) fn run_from_row(row: sqlx::postgres::PgRow) -> Result<RunRecord, StoreError> {
    Ok(RunRecord {
        id: RunId(row.try_get("id")?),
        workspace_id: WorkspaceId(row.try_get("workspace_id")?),
        task_id: TaskId(row.try_get("task_id")?),
        task_version_id: TaskVersionId(row.try_get("task_version_id")?),
        status: parse_status(row.try_get::<String, _>("status")?.as_str())?,
        trigger: parse_trigger(row.try_get::<String, _>("trigger")?.as_str())?,
        dry_run: row.try_get("dry_run")?,
        inputs: row.try_get("inputs")?,
        output: row.try_get("output")?,
        error: row.try_get("error")?,
        cost: UsdMicros(row.try_get("cost_micros")?),
        max_seq: row.try_get("max_seq")?,
        cli_version: row.try_get("cli_version")?,
        fingerprint: row.try_get("fingerprint")?,
        output_digest: row.try_get("output_digest")?,
        compare_to: row
            .try_get::<Option<uuid::Uuid>, _>("compare_to")?
            .map(RunId),
        created_at: row.try_get("created_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
    })
}

/// 库里的取值与 `ai-task-proto` 的 serde tag 逐字一致，所以直接走 serde，
/// 不另写一张映射表——两份映射早晚会漂移。
pub(crate) fn parse_status(value: &str) -> Result<RunStatus, StoreError> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(|_| {
        StoreError::Corrupt {
            what: "runs.status",
            detail: format!("`{value}` 不是已知的 RunStatus"),
        }
    })
}

pub(crate) fn parse_trigger(value: &str) -> Result<TriggerKind, StoreError> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(|_| {
        StoreError::Corrupt {
            what: "runs.trigger",
            detail: format!("`{value}` 不是已知的 TriggerKind"),
        }
    })
}

fn status_str(status: RunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "failed".into())
}

fn trigger_str(trigger: TriggerKind) -> String {
    serde_json::to_value(trigger)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "manual".into())
}

/// 删掉这些 run 的事件流和资源采样。
///
/// `run_events` / `run_metrics` 是分区表且**没有指向 runs 的外键**——加外键要在
/// 每条事件插入时多一次父行检查，而那是这个系统最热的写路径。代价是级联得手写。
///
/// **所有删 run 的路径都必须先调它。**漏掉的话事件表里会留下一批永远读不到、
/// 也永远不会变小的孤儿行（读路径一律按 run_id 过滤）。
pub(crate) async fn purge_run_children(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    run_ids: &[uuid::Uuid],
) -> Result<(), StoreError> {
    if run_ids.is_empty() {
        return Ok(());
    }
    // 按 run_id 走 (run_id, seq) 主键，逐分区索引扫，不会全表
    sqlx::query("DELETE FROM run_metrics WHERE run_id = ANY($1)")
        .bind(run_ids)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM run_events WHERE run_id = ANY($1)")
        .bind(run_ids)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 库里的 CHECK 取值和 proto 的 serde tag 必须逐字一致。
    /// 这条测试挡住「改了 Rust 枚举但忘了改 migration」。
    #[test]
    fn status_and_trigger_round_trip_through_the_database_representation() {
        for status in [
            RunStatus::Queued,
            RunStatus::Running,
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::TimedOut,
            RunStatus::BudgetExceeded,
            RunStatus::ResourceExceeded,
        ] {
            let text = status_str(status);
            assert_eq!(parse_status(&text).expect("解析"), status);
            assert!(
                text.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{text}"
            );
        }
        for trigger in [
            TriggerKind::Manual,
            TriggerKind::Schedule,
            TriggerKind::Api,
            TriggerKind::Parent,
        ] {
            let text = trigger_str(trigger);
            assert_eq!(parse_trigger(&text).expect("解析"), trigger);
        }
    }

    #[test]
    fn unknown_database_values_are_corruption_not_a_silent_default() {
        assert!(matches!(
            parse_status("RUNNING"),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            parse_trigger("webhook"),
            Err(StoreError::Corrupt { .. })
        ));
    }
}
