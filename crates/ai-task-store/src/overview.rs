//! 首页那一屏的聚合查询。
//!
//! **分成五条独立的查询，而不是一个 CTE 巨兽。** 每条都能单独 EXPLAIN、
//! 单独命中一个索引；拼成一条之后 planner 的选择就不再由人控制，
//! 而首页是全站唯一一个每隔几秒就打一次的接口。
//!
//! 窗口聚合必须在这里做：之前是客户端拉最近 200 条 run 自己算，
//! 第 201 条之后的就静静地漏掉了——界面上那几个数字看不出自己是错的。

use ai_task_proto::{
    DailyBucket, OutcomeCounts, OverviewRun, OverviewStats, RunId, ScheduleId, TaskHealth, TaskId,
    UpcomingFire, UsdMicros, WorkspaceId,
};
use chrono::{NaiveDate, Utc};
use sqlx::Row;

use crate::pool::{Store, StoreError};
use crate::runs::{parse_status, parse_trigger};

/// 失败类终态，和前端 `FAILED_STATUSES` 必须一致。
///
/// 好几条查询都要这一段。用宏展开成字面量、再 `concat!` 进 SQL，
/// 而不是 `format!` 拼：拼出来的仍是一条完全静态的 SQL，不存在注入这个问题。
macro_rules! failed_statuses {
    () => {
        "('failed', 'timed_out', 'budget_exceeded', 'resource_exceeded')"
    };
}

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

        let row = sqlx::query(concat!(
            "SELECT
               count(*) FILTER (WHERE created_at >= $2)                       AS runs,
               count(*) FILTER (WHERE created_at >= $2 AND status IN ",
            failed_statuses!(),
            ")                                                               AS failed,
               count(*) FILTER (WHERE created_at >= $2 AND NOT dry_run
                 AND status = 'succeeded')                                    AS real_succeeded,
               count(*) FILTER (WHERE created_at >= $2 AND NOT dry_run AND status IN ",
            failed_statuses!(),
            ")                                                               AS real_failed,
               -- sum(bigint) 在 Postgres 里返回 numeric，不加这个 cast 解不出 i64
               coalesce(sum(cost_micros) FILTER (WHERE created_at >= $2), 0)::bigint AS spend,
               count(*) FILTER (WHERE status = 'running')                     AS running,
               count(*) FILTER (WHERE status = 'queued')                      AS queued
             FROM runs
             WHERE workspace_id = $1 AND (created_at >= $2 OR status IN ('queued', 'running'))"
        ))
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
            outcomes: OutcomeCounts {
                succeeded: row.try_get("real_succeeded")?,
                failed: row.try_get("real_failed")?,
            },
        })
    }

    /// 每天的执行结果，按 `timezone` 的日历日分桶，`first_day..=last_day` 每天一行，没有执行的补零。
    ///
    /// 范围条件写成"本地零点换算成的时刻"，让它落在 `runs_recent_idx` 上成为一次范围扫描；
    /// 夏令时切换的那天也由 `AT TIME ZONE` 自己算对。实测一年、10 万条历史里取 14 天约 5ms。
    pub async fn overview_daily(
        &self,
        workspace_id: WorkspaceId,
        timezone: &str,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<Vec<DailyBucket>, StoreError> {
        let rows = sqlx::query(concat!(
            "WITH agg AS (
               SELECT (r.created_at AT TIME ZONE $2)::date AS day,
                      count(*) FILTER (WHERE NOT r.dry_run) AS runs,
                      count(*) FILTER (WHERE NOT r.dry_run AND r.status = 'succeeded') AS succeeded,
                      count(*) FILTER (WHERE NOT r.dry_run AND r.status IN ",
            failed_statuses!(),
            ") AS failed,
                      coalesce(sum(r.cost_micros), 0)::bigint AS spend
               FROM runs r
               WHERE r.workspace_id = $1
                 AND r.created_at >= $3::date::timestamp AT TIME ZONE $2
                 AND r.created_at < ($4::date + 1)::timestamp AT TIME ZONE $2
               GROUP BY 1
             )
             SELECT d.day::date AS day,
                    coalesce(a.runs, 0) AS runs,
                    coalesce(a.succeeded, 0) AS succeeded,
                    coalesce(a.failed, 0) AS failed,
                    coalesce(a.spend, 0) AS spend
             FROM generate_series($3::date, $4::date, interval '1 day') AS d(day)
             LEFT JOIN agg a ON a.day = d.day::date
             ORDER BY d.day"
        ))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(timezone)
        .bind(first_day)
        .bind(last_day)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(DailyBucket {
                    date: row.try_get("day")?,
                    runs: row.try_get("runs")?,
                    succeeded: row.try_get("succeeded")?,
                    failed: row.try_get("failed")?,
                    spend_usd: UsdMicros(row.try_get("spend")?),
                })
            })
            .collect()
    }

    /// 窗口内有失败的任务，失败多的在前。影子执行不算。
    ///
    /// 走 `runs_recent_idx` 的范围扫描再按任务聚合；一年、10 万条历史里看 7 天约 8ms。
    pub async fn overview_failing_tasks(
        &self,
        workspace_id: WorkspaceId,
        window_hours: u32,
        limit: i64,
    ) -> Result<Vec<TaskHealth>, StoreError> {
        let since = Utc::now() - chrono::Duration::hours(i64::from(window_hours));
        let rows = sqlx::query(concat!(
            "SELECT r.task_id, t.name AS task_name,
                    count(*) AS runs,
                    count(*) FILTER (WHERE r.status IN ",
            failed_statuses!(),
            ") AS failed,
                    coalesce(sum(r.cost_micros), 0)::bigint AS spend,
                    (array_agg(r.id ORDER BY r.created_at DESC, r.id DESC))[1] AS last_run_id,
                    (array_agg(r.status ORDER BY r.created_at DESC, r.id DESC))[1] AS last_status,
                    max(r.created_at) AS last_run_at,
                    max(r.created_at) FILTER (WHERE r.status IN ",
            failed_statuses!(),
            ") AS last_failed_at,
                    (array_agg(left(r.error, 200) ORDER BY r.created_at DESC, r.id DESC)
                       FILTER (WHERE r.status IN ",
            failed_statuses!(),
            "))[1] AS last_error
             FROM runs r
             JOIN tasks t ON t.id = r.task_id
             WHERE r.workspace_id = $1 AND r.created_at >= $2 AND NOT r.dry_run
             GROUP BY r.task_id, t.name
             HAVING count(*) FILTER (WHERE r.status IN ",
            failed_statuses!(),
            ") > 0
             ORDER BY failed DESC, runs ASC, t.name
             LIMIT $3"
        ))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(since)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(TaskHealth {
                    task_id: TaskId(row.try_get("task_id")?),
                    task_name: row.try_get("task_name")?,
                    runs: row.try_get("runs")?,
                    failed: row.try_get("failed")?,
                    spend_usd: UsdMicros(row.try_get("spend")?),
                    last_run_id: RunId(row.try_get("last_run_id")?),
                    last_status: parse_status(row.try_get::<String, _>("last_status")?.as_str())?,
                    last_run_at: row.try_get("last_run_at")?,
                    last_failed_at: row.try_get("last_failed_at")?,
                    last_error: row.try_get("last_error")?,
                })
            })
            .collect()
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
