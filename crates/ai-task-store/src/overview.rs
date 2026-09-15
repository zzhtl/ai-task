//! 首页那一屏的聚合查询。
//!
//! **分成五条独立的查询，而不是一个 CTE 巨兽。** 每条都能单独 EXPLAIN、
//! 单独命中一个索引；拼成一条之后 planner 的选择就不再由人控制，
//! 而首页是全站唯一一个每隔几秒就打一次的接口。
//!
//! 窗口聚合必须在这里做：之前是客户端拉最近 200 条 run 自己算，
//! 第 201 条之后的就静静地漏掉了——界面上那几个数字看不出自己是错的。

use ai_task_proto::{
    OverviewRun, OverviewStats, RunId, ScheduleId, TaskId, UpcomingFire, UsdMicros, WorkspaceId,
};
use chrono::Utc;
use sqlx::Row;

use crate::pool::{Store, StoreError};
use crate::runs::{parse_status, parse_trigger};

impl Store {
    /// 顶部那一排指标。
    ///
    /// 窗口内的三个数（次数 / 失败 / 花费）走 `runs_recent_idx`（workspace, created_at DESC）；
    /// 当前在跑和排队走部分索引 `runs_active_idx`。
    pub async fn overview_stats(
        &self,
        workspace_id: WorkspaceId,
        window_hours: u32,
    ) -> Result<OverviewStats, StoreError> {
        let since = Utc::now() - chrono::Duration::hours(i64::from(window_hours));

        // 失败类终态写死在 SQL 里，和前端 FAILED_STATUSES 必须一致。
        // 不用 format! 拼：一条完全静态的 SQL 连"要不要审计注入"这个问题都不存在。
        let row = sqlx::query(
            "SELECT
               count(*) FILTER (WHERE created_at >= $2)                       AS runs,
               count(*) FILTER (WHERE created_at >= $2
                 AND status IN ('failed', 'timed_out',
                                'budget_exceeded', 'resource_exceeded'))      AS failed,
               -- sum(bigint) 在 Postgres 里返回 numeric，不加这个 cast 解不出 i64
               coalesce(sum(cost_micros) FILTER (WHERE created_at >= $2), 0)::bigint AS spend,
               count(*) FILTER (WHERE status = 'running')                     AS running,
               count(*) FILTER (WHERE status = 'queued')                      AS queued
             FROM runs
             WHERE workspace_id = $1 AND (created_at >= $2 OR status IN ('queued', 'running'))",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(since)
        .fetch_one(self.pool())
        .await?;

        let approvals: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM approvals a
             JOIN runs r ON r.id = a.run_id
             WHERE r.workspace_id = $1 AND a.decided_at IS NULL AND a.expires_at > now()",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_one(self.pool())
        .await?;

        let tasks: i64 = sqlx::query_scalar("SELECT count(*) FROM tasks WHERE workspace_id = $1")
            .bind(uuid::Uuid::from(workspace_id))
            .fetch_one(self.pool())
            .await?;

        Ok(OverviewStats {
            window_hours,
            runs: row.try_get("runs")?,
            failed: row.try_get("failed")?,
            spend_usd: UsdMicros(row.try_get("spend")?),
            running: row.try_get("running")?,
            queued: row.try_get("queued")?,
            pending_approvals: approvals,
            tasks,
        })
    }

    /// 在跑的和排队的。走 `runs_active_idx` 这个部分索引。
    pub async fn overview_live(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<OverviewRun>, StoreError> {
        self.overview_runs(
            workspace_id,
            "r.status IN ('queued', 'running')",
            "r.created_at DESC",
            limit,
        )
        .await
    }

    /// 最近结束的。
    pub async fn overview_recent(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<OverviewRun>, StoreError> {
        self.overview_runs(
            workspace_id,
            "r.status NOT IN ('queued', 'running')",
            "r.created_at DESC, r.id DESC",
            limit,
        )
        .await
    }

    /// 接下来会自己响的定时。停用的任务不会响，过滤掉。
    pub async fn overview_upcoming(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<UpcomingFire>, StoreError> {
        let rows = sqlx::query(
            "SELECT s.id, s.task_id, s.cron, s.next_fire_at, t.name AS task_name
             FROM schedules s
             JOIN tasks t ON t.id = s.task_id
             WHERE s.workspace_id = $1
               AND s.enabled AND t.enabled
               AND s.next_fire_at IS NOT NULL
             ORDER BY s.next_fire_at
             LIMIT $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(UpcomingFire {
                    schedule_id: ScheduleId(row.try_get("id")?),
                    task_id: TaskId(row.try_get("task_id")?),
                    task_name: row.try_get("task_name")?,
                    cron: row.try_get("cron")?,
                    next_fire_at: row.try_get("next_fire_at")?,
                })
            })
            .collect()
    }

    /// 两个 run 列表共用的查询。`predicate` / `order` 只能是代码里的字面量。
    async fn overview_runs(
        &self,
        workspace_id: WorkspaceId,
        predicate: &'static str,
        order: &'static str,
        limit: i64,
    ) -> Result<Vec<OverviewRun>, StoreError> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT r.id, r.task_id, r.status, r.trigger, r.dry_run, r.error, r.cost_micros,
                    r.created_at, r.started_at, r.finished_at, t.name AS task_name
             FROM runs r
             JOIN tasks t ON t.id = r.task_id
             WHERE r.workspace_id = $1 AND {predicate}
             ORDER BY {order}
             LIMIT $2"
        )))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(OverviewRun {
                    id: RunId(row.try_get("id")?),
                    task_id: TaskId(row.try_get("task_id")?),
                    task_name: row.try_get("task_name")?,
                    status: parse_status(row.try_get::<String, _>("status")?.as_str())?,
                    trigger: parse_trigger(row.try_get::<String, _>("trigger")?.as_str())?,
                    dry_run: row.try_get("dry_run")?,
                    created_at: row.try_get("created_at")?,
                    started_at: row.try_get("started_at")?,
                    finished_at: row.try_get("finished_at")?,
                    cost_usd: UsdMicros(row.try_get("cost_micros")?),
                    error: row.try_get("error")?,
                })
            })
            .collect()
    }
}
