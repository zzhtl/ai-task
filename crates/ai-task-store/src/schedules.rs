//! 定时配置的读写。
//!
//! 这里只提供**吃事务的原语**，编排逻辑（算下个触发点、misfire 补偿、
//! overlap 判定）留在 `ai-task-runtime`：store 不应该知道 cron 是什么。

use ai_task_proto::{
    MisfirePolicy, OverlapPolicy, RunEventBody, RunId, ScheduleId, TaskId, TaskVersionId,
    TriggerKind, WorkspaceId,
};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Row, Transaction};

use crate::events::{PendingEvent, append_in_tx};
use crate::{Store, StoreError};

/// 一条到期待处理的定时配置。
#[derive(Debug, Clone)]
pub struct DueSchedule {
    pub id: ScheduleId,
    pub workspace_id: WorkspaceId,
    pub task_id: TaskId,
    /// 任务当前生效的版本。run 绑的是它，不是任务本身。
    pub task_version_id: TaskVersionId,
    pub task_enabled: bool,
    pub cron: String,
    pub timezone: String,
    pub misfire: MisfirePolicy,
    pub overlap: OverlapPolicy,
    pub jitter_s: u32,
    /// 规范触发点。
    pub next_fire_at: Option<DateTime<Utc>>,
    /// 上次真正产生 run 的规范触发点。misfire 补偿从它往后算。
    pub last_fired_at: Option<DateTime<Utc>>,
}

/// 一次处理后要写回的调度状态。
#[derive(Debug, Clone, Copy)]
pub struct SchedulePosition {
    pub next_fire_at: DateTime<Utc>,
    /// 规范触发点 + 抖动。调度器比较的是它。
    pub next_claim_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
}

/// 建定时配置的入参。
#[derive(Debug, Clone)]
pub struct NewSchedule {
    pub workspace_id: WorkspaceId,
    pub task_id: TaskId,
    pub cron: String,
    pub timezone: String,
    pub misfire: MisfirePolicy,
    pub overlap: OverlapPolicy,
    pub jitter_s: u32,
    pub enabled: bool,
    pub next_fire_at: DateTime<Utc>,
    pub next_claim_at: DateTime<Utc>,
}

impl Store {
    /// 开一个事务并领取**一条**到期的定时配置。
    ///
    /// `FOR UPDATE SKIP LOCKED`：多副本同时轮询时各自拿到不同的行，不需要选主，
    /// 也不会互相阻塞。返回 `None` 表示当前没有到期的。
    ///
    /// 调用方必须在同一事务里处理完并提交——事务里只有纯 CPU 计算
    /// （算下个触发点），没有任何网络 IO。
    pub async fn claim_due_schedule(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<(Transaction<'static, Postgres>, DueSchedule)>, StoreError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT s.id, s.workspace_id, s.task_id, s.cron, s.timezone, s.misfire, s.overlap,
                    s.jitter_s, s.next_fire_at, s.last_fired_at,
                    t.current_version_id, t.enabled AS task_enabled
             FROM schedules s
             JOIN tasks t ON t.id = s.task_id
             WHERE s.enabled AND s.next_claim_at <= $1
             ORDER BY s.next_claim_at
             LIMIT 1
             FOR UPDATE OF s SKIP LOCKED",
        )
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = row else {
            // 没领到就把事务丢掉，别占着连接
            tx.rollback().await?;
            return Ok(None);
        };

        Ok(Some((
            tx,
            DueSchedule {
                id: ScheduleId(row.try_get("id")?),
                workspace_id: WorkspaceId(row.try_get("workspace_id")?),
                task_id: TaskId(row.try_get("task_id")?),
                task_version_id: TaskVersionId(row.try_get("current_version_id")?),
                task_enabled: row.try_get("task_enabled")?,
                cron: row.try_get("cron")?,
                timezone: row.try_get("timezone")?,
                misfire: parse_enum(
                    row.try_get::<String, _>("misfire")?.as_str(),
                    "schedules.misfire",
                )?,
                overlap: parse_enum(
                    row.try_get::<String, _>("overlap")?.as_str(),
                    "schedules.overlap",
                )?,
                jitter_s: u32::try_from(row.try_get::<i32, _>("jitter_s")?).unwrap_or(0),
                next_fire_at: row.try_get("next_fire_at")?,
                last_fired_at: row.try_get("last_fired_at")?,
            },
        )))
    }

    pub async fn create_schedule(&self, new: NewSchedule) -> Result<ScheduleId, StoreError> {
        let id = ScheduleId::new();
        sqlx::query(
            "INSERT INTO schedules
                (id, workspace_id, task_id, cron, timezone, misfire, overlap, jitter_s,
                 enabled, next_fire_at, next_claim_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(uuid::Uuid::from(new.task_id))
        .bind(&new.cron)
        .bind(&new.timezone)
        .bind(enum_str(new.misfire))
        .bind(enum_str(new.overlap))
        .bind(i32::try_from(new.jitter_s).unwrap_or(0))
        .bind(new.enabled)
        .bind(new.next_fire_at)
        .bind(new.next_claim_at)
        .execute(self.pool())
        .await?;
        Ok(id)
    }

    /// 改一条定时配置。`next_fire_at` 由调用方按新表达式算好。
    ///
    /// `next_claim_at` 跟着一起改：它是加了 jitter 的抢占时刻，不改的话
    /// 新表达式要等到下一个旧触发点之后才真正生效。
    pub async fn update_schedule(
        &self,
        workspace_id: WorkspaceId,
        id: ScheduleId,
        new: &NewSchedule,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "UPDATE schedules
             SET cron = $3, timezone = $4, misfire = $5, overlap = $6, jitter_s = $7,
                 enabled = $8, next_fire_at = $9, next_claim_at = $10, updated_at = now()
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(&new.cron)
        .bind(&new.timezone)
        .bind(enum_str(new.misfire))
        .bind(enum_str(new.overlap))
        .bind(i32::try_from(new.jitter_s).unwrap_or(0))
        .bind(new.enabled)
        .bind(new.next_fire_at)
        .bind(new.next_claim_at)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// 启用/停用。停用不删记录，`next_fire_at` 原样保留。
    pub async fn set_schedule_enabled(
        &self,
        workspace_id: WorkspaceId,
        id: ScheduleId,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "UPDATE schedules SET enabled = $3, updated_at = now()
                 WHERE id = $1 AND workspace_id = $2",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(enabled)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn delete_schedule(
        &self,
        workspace_id: WorkspaceId,
        id: ScheduleId,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM schedules WHERE id = $1 AND workspace_id = $2")
            .bind(uuid::Uuid::from(id))
            .bind(uuid::Uuid::from(workspace_id))
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn list_schedules(
        &self,
        workspace_id: WorkspaceId,
        task_id: Option<TaskId>,
    ) -> Result<Vec<(ScheduleId, DueSchedule, bool)>, StoreError> {
        let rows = sqlx::query(
            "SELECT s.id, s.workspace_id, s.task_id, s.cron, s.timezone, s.misfire, s.overlap,
                    s.jitter_s, s.next_fire_at, s.last_fired_at, s.enabled,
                    t.current_version_id, t.enabled AS task_enabled
             FROM schedules s
             JOIN tasks t ON t.id = s.task_id
             WHERE s.workspace_id = $1 AND ($2::uuid IS NULL OR s.task_id = $2)
             ORDER BY s.next_fire_at NULLS LAST
             LIMIT $3",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(task_id.map(uuid::Uuid::from))
        .bind(crate::pool::CONFIG_LIST_CAP)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let id = ScheduleId(row.try_get("id")?);
                let enabled: bool = row.try_get("enabled")?;
                Ok((
                    id,
                    DueSchedule {
                        id,
                        workspace_id: WorkspaceId(row.try_get("workspace_id")?),
                        task_id: TaskId(row.try_get("task_id")?),
                        task_version_id: TaskVersionId(row.try_get("current_version_id")?),
                        task_enabled: row.try_get("task_enabled")?,
                        cron: row.try_get("cron")?,
                        timezone: row.try_get("timezone")?,
                        misfire: parse_enum(
                            row.try_get::<String, _>("misfire")?.as_str(),
                            "schedules.misfire",
                        )?,
                        overlap: parse_enum(
                            row.try_get::<String, _>("overlap")?.as_str(),
                            "schedules.overlap",
                        )?,
                        jitter_s: u32::try_from(row.try_get::<i32, _>("jitter_s")?).unwrap_or(0),
                        next_fire_at: row.try_get("next_fire_at")?,
                        last_fired_at: row.try_get("last_fired_at")?,
                    },
                    enabled,
                ))
            })
            .collect()
    }
}

/// 在事务里把调度状态推进到下一个位置。
pub async fn advance_schedule_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: ScheduleId,
    position: SchedulePosition,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE schedules
         SET next_fire_at = $2, next_claim_at = $3,
             last_fired_at = COALESCE($4, last_fired_at),
             last_claimed_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(uuid::Uuid::from(id))
    .bind(position.next_fire_at)
    .bind(position.next_claim_at)
    .bind(position.last_fired_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 在事务里为一个定时触发点建 run。
///
/// 返回 `None` 表示这个触发点已经有 run 了（另一个副本抢先了）。
/// **幂等靠 `UNIQUE (schedule_id, fire_at)` 这个约束，不靠锁**——双触发在
/// 结构上就不可能，即使两个副本的时钟有偏差、即使有人手动重放。
pub async fn create_scheduled_run_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    schedule: &DueSchedule,
    fire_at: DateTime<Utc>,
) -> Result<Option<RunId>, StoreError> {
    let run_id = RunId::new();
    let inserted = sqlx::query(
        "INSERT INTO runs
            (id, workspace_id, task_id, task_version_id, schedule_id, fire_at, trigger, status)
         VALUES ($1, $2, $3, $4, $5, $6, 'schedule', 'queued')
         ON CONFLICT (schedule_id, fire_at) DO NOTHING
         RETURNING id",
    )
    .bind(uuid::Uuid::from(run_id))
    .bind(uuid::Uuid::from(schedule.workspace_id))
    .bind(uuid::Uuid::from(schedule.task_id))
    .bind(uuid::Uuid::from(schedule.task_version_id))
    .bind(uuid::Uuid::from(schedule.id))
    .bind(fire_at)
    .fetch_optional(&mut **tx)
    .await?;

    if inserted.is_none() {
        return Ok(None);
    }

    append_in_tx(
        tx,
        run_id,
        &[PendingEvent::run(RunEventBody::RunQueued {
            task_version_id: schedule.task_version_id,
            trigger: TriggerKind::Schedule,
            inputs: None,
            dry_run: false,
        })],
    )
    .await?;
    Ok(Some(run_id))
}

/// 这个任务当前有没有还没跑完的 run。overlap 策略要用。
pub async fn has_active_run_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: WorkspaceId,
    task_id: TaskId,
) -> Result<bool, StoreError> {
    // **workspace_id 不能省。** `runs_active_idx` 是
    // `(workspace_id, created_at) WHERE status IN ('queued','running')`——
    // 不带前导列就用不上这个部分索引，而这条查询是每个 tick、每条
    // overlap=skip|queue 的定时都要跑一次的。
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM runs
             WHERE workspace_id = $1 AND task_id = $2 AND status IN ('queued','running')
         )",
    )
    .bind(uuid::Uuid::from(workspace_id))
    .bind(uuid::Uuid::from(task_id))
    .fetch_one(&mut **tx)
    .await?;
    Ok(exists)
}

/// 库里的取值与 proto 的 serde tag 逐字一致，直接走 serde，不另写映射表。
fn parse_enum<T: serde::de::DeserializeOwned>(
    value: &str,
    what: &'static str,
) -> Result<T, StoreError> {
    serde_json::from_value(serde_json::Value::String(value.into())).map_err(|_| {
        StoreError::Corrupt {
            what,
            detail: format!("`{value}` 不是已知取值"),
        }
    })
}

fn enum_str<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_enums_round_trip_through_the_database_representation() {
        for p in [
            MisfirePolicy::Skip,
            MisfirePolicy::FireOnce,
            MisfirePolicy::FireAll,
        ] {
            let s = enum_str(p);
            assert_eq!(parse_enum::<MisfirePolicy>(&s, "x").expect("解析"), p);
        }
        for p in [
            OverlapPolicy::Allow,
            OverlapPolicy::Skip,
            OverlapPolicy::Queue,
        ] {
            let s = enum_str(p);
            assert_eq!(parse_enum::<OverlapPolicy>(&s, "x").expect("解析"), p);
        }
    }

    #[test]
    fn unknown_policy_values_are_corruption_not_a_silent_default() {
        assert!(matches!(
            parse_enum::<MisfirePolicy>("FIRE_ONCE", "x"),
            Err(StoreError::Corrupt { .. })
        ));
    }
}
