//! cron 调度器。
//!
//! **多副本无选主**：每个副本轮询时用 `FOR UPDATE SKIP LOCKED` 各自领一条到期
//! 的配置，互不阻塞也不需要协调。真正保证「一个触发点只产生一个 run」的是
//! `UNIQUE (schedule_id, fire_at)` 这个约束——即使两个副本时钟有偏差、
//! 即使有人手动重放，双触发在结构上就不可能。
//!
//! 事务边界：领取、建 run、推进游标在**同一个事务**里完成。事务内只有纯 CPU
//! 计算（算下个触发点），没有任何网络 IO。

use std::time::Duration;

use ai_task_proto::{OverlapPolicy, RunId, WorkspaceId};
use ai_task_store::schedules::{
    DueSchedule, SchedulePosition, advance_schedule_in_tx, create_scheduled_run_in_tx,
    has_active_run_in_tx,
};
use ai_task_store::{Store, StoreError};
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::cron::{CronSchedule, apply_jitter};

/// 触发点迟到多久算「错过」。
///
/// 之内的算准点，**任何 misfire 策略都要跑**——否则 `skip` 在正常运行时会
/// 退化成「永远不触发」。
const GRACE: chrono::Duration = chrono::Duration::seconds(30);

/// cron 表达式解析失败时，把这条配置往后推多久再试。
///
/// 不推的话它每个 tick 都会被领一次、失败一次，把日志刷满。
/// 也不自动停用它——用户没让停，静默停用比刷日志更糟。
const BAD_EXPRESSION_BACKOFF: chrono::Duration = chrono::Duration::minutes(5);

/// `overlap = queue` 且上一次还没跑完时，隔多久再看一眼。
const QUEUE_RETRY: chrono::Duration = chrono::Duration::seconds(10);

/// 一次轮询的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tick {
    /// 本次处理了多少条配置。
    pub processed: usize,
    /// 新建的 run，调用方负责真正把它们跑起来。
    pub created: Vec<(WorkspaceId, RunId)>,
    /// 因为 overlap 策略被推迟的配置。
    pub deferred: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<sqlx::Error> for SchedulerError {
    fn from(err: sqlx::Error) -> Self {
        Self::Store(StoreError::Query(err))
    }
}

/// 定时调度器。
#[derive(Clone)]
pub struct Scheduler {
    store: Store,
}

impl Scheduler {
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// 处理当前所有到期的配置。
    ///
    /// 一条一个事务：一条坏配置不该拖累其它的。
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<Tick, SchedulerError> {
        let mut tick = Tick::default();
        // 单次轮询的处理上限。到期的配置很多时分几轮处理，
        // 免得一个 tick 把连接池占满。
        for _ in 0..64 {
            match self.process_one(now).await? {
                Some(outcome) => {
                    tick.processed += 1;
                    match outcome {
                        Outcome::Created(runs) => tick.created.extend(runs),
                        Outcome::Deferred => tick.deferred += 1,
                        Outcome::Nothing => {}
                    }
                }
                None => break,
            }
        }
        Ok(tick)
    }

    /// 领取并处理一条。返回 `None` 表示当前没有到期的。
    async fn process_one(&self, now: DateTime<Utc>) -> Result<Option<Outcome>, SchedulerError> {
        let Some((mut tx, schedule)) = self.store.claim_due_schedule(now).await? else {
            return Ok(None);
        };

        let cron = match CronSchedule::parse(&schedule.cron, &schedule.timezone) {
            Ok(cron) => cron,
            Err(err) => {
                // 表达式在保存时校验过；能走到这里说明是直接改库或者升级后
                // 语义变了。往后推一段再试，别每个 tick 都刷一次日志。
                tracing::error!(
                    schedule_id = %schedule.id, cron = %schedule.cron, error = %err,
                    "定时配置的表达式无法解析，已推迟重试"
                );
                advance_schedule_in_tx(
                    &mut tx,
                    schedule.id,
                    SchedulePosition {
                        next_fire_at: schedule.next_fire_at.unwrap_or(now),
                        next_claim_at: now + BAD_EXPRESSION_BACKOFF,
                        last_fired_at: None,
                    },
                )
                .await?;
                tx.commit().await?;
                return Ok(Some(Outcome::Nothing));
            }
        };

        // 从上次真正触发过的点往后枚举。从没触发过就从这次的规范触发点前一瞬开始，
        // 好把它本身也算进来。
        let anchor = schedule.last_fired_at.unwrap_or_else(|| {
            schedule
                .next_fire_at
                .unwrap_or(now)
                .checked_sub_signed(chrono::Duration::milliseconds(1))
                .unwrap_or(now)
        });
        let due = cron.due_between(anchor, now);

        if due.truncated {
            tracing::warn!(
                schedule_id = %schedule.id, missed = due.total, kept = due.points.len(),
                "错过的触发点过多，只补最近的一批"
            );
        }

        // overlap = queue 且上一次还没跑完：**不推进游标**，隔一会儿再看。
        // 这样这个触发点会一直留在待处理集合里，等上一次结束后自然补上。
        if schedule.overlap == OverlapPolicy::Queue
            && !due.is_empty()
            && has_active_run_in_tx(&mut tx, schedule.task_id).await?
        {
            advance_schedule_in_tx(
                &mut tx,
                schedule.id,
                SchedulePosition {
                    next_fire_at: schedule.next_fire_at.unwrap_or(now),
                    next_claim_at: now + QUEUE_RETRY,
                    last_fired_at: None,
                },
            )
            .await?;
            tx.commit().await?;
            return Ok(Some(Outcome::Deferred));
        }

        let mut fires = due.select(now, schedule.misfire, GRACE);

        // 任务被停用 / overlap = skip 且有在跑的：不建 run，但游标照常推进。
        // 不推进的话下一个 tick 会把同一批触发点再算一遍，白白空转。
        if !schedule.task_enabled {
            tracing::debug!(schedule_id = %schedule.id, "任务已停用，跳过本次触发");
            fires.clear();
        } else if schedule.overlap == OverlapPolicy::Skip
            && !fires.is_empty()
            && has_active_run_in_tx(&mut tx, schedule.task_id).await?
        {
            tracing::info!(schedule_id = %schedule.id, "上一次还没跑完，按 skip 策略丢弃本次触发");
            fires.clear();
        }

        let mut created = Vec::new();
        for fire_at in &fires {
            match create_scheduled_run_in_tx(&mut tx, &schedule, *fire_at).await? {
                Some(run_id) => created.push((schedule.workspace_id, run_id)),
                // 另一个副本抢先建了。这不是错误，正是唯一约束在起作用。
                None => tracing::debug!(
                    schedule_id = %schedule.id, %fire_at,
                    "这个触发点已经有 run 了，让给先到的副本"
                ),
            }
        }

        let position = next_position(&cron, &schedule, &due.newest(), now);
        advance_schedule_in_tx(&mut tx, schedule.id, position).await?;
        tx.commit().await?;

        Ok(Some(if created.is_empty() {
            Outcome::Nothing
        } else {
            Outcome::Created(created)
        }))
    }

    /// 后台轮询循环。
    ///
    /// `on_created` 在**事务提交之后**调用，用来把新建的 run 真正跑起来。
    /// 放在事务外是刻意的：执行是长耗时的网络 IO，不能占着数据库事务。
    pub fn spawn<F>(
        self,
        interval: Duration,
        on_created: F,
        shutdown: CancellationToken,
    ) -> tokio::task::JoinHandle<()>
    where
        F: Fn(WorkspaceId, RunId) + Send + Sync + 'static,
    {
        tokio::spawn(async move {
            tracing::info!(interval_ms = interval.as_millis(), "调度器已启动");
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    () = tokio::time::sleep(interval) => {}
                }

                match self.tick(Utc::now()).await {
                    Ok(tick) => {
                        for (workspace_id, run_id) in tick.created {
                            on_created(workspace_id, run_id);
                        }
                    }
                    Err(err) => {
                        // 数据库抖一下不该让调度器退出——下一个 tick 再试。
                        tracing::error!(error = %err, "调度轮询失败，下个周期重试");
                    }
                }
            }
            tracing::info!("调度器已停止");
        })
    }
}

enum Outcome {
    Created(Vec<(WorkspaceId, RunId)>),
    Deferred,
    Nothing,
}

/// 算出游标的下一个位置。
fn next_position(
    cron: &CronSchedule,
    schedule: &DueSchedule,
    newest_due: &Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> SchedulePosition {
    // 从「已处理到的最新触发点」和 now 里取靠后的那个往下找，
    // 保证新的 next_fire_at 一定在未来。
    let from = newest_due.map_or(now, |newest| newest.max(now));
    let next_fire_at = cron
        .next_after(from)
        .unwrap_or(from + chrono::Duration::hours(1));

    SchedulePosition {
        next_fire_at,
        // 抖动只影响领取时刻，绝不能进 next_fire_at ——后者要原样写进
        // runs.fire_at，加了随机量唯一约束就废了
        next_claim_at: apply_jitter(next_fire_at, schedule.jitter_s),
        // 无论有没有真的建 run，都要把游标推到最新的到期触发点，
        // 否则下一个 tick 会把同一批再算一遍
        last_fired_at: *newest_due,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_task_proto::{MisfirePolicy, ScheduleId};

    fn schedule(jitter_s: u32) -> DueSchedule {
        DueSchedule {
            id: ScheduleId::new(),
            workspace_id: WorkspaceId::new(),
            task_id: ai_task_proto::TaskId::new(),
            task_version_id: ai_task_proto::TaskVersionId::new(),
            task_enabled: true,
            cron: "*/1 * * * *".into(),
            timezone: "UTC".into(),
            misfire: MisfirePolicy::FireOnce,
            overlap: OverlapPolicy::Skip,
            jitter_s,
            next_fire_at: None,
            last_fired_at: None,
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("时间格式")
            .with_timezone(&Utc)
    }

    #[test]
    fn the_next_fire_point_is_always_in_the_future() {
        let cron = CronSchedule::parse("*/1 * * * *", "UTC").expect("解析");
        let now = at("2026-09-08T10:03:20Z");
        // 即使刚补完一批很老的触发点，下一个也必须落在 now 之后
        let position = next_position(&cron, &schedule(0), &Some(at("2026-09-08T09:00:00Z")), now);
        assert!(position.next_fire_at > now, "{:?}", position.next_fire_at);
        assert_eq!(position.next_fire_at, at("2026-09-08T10:04:00Z"));
    }

    #[test]
    fn the_cursor_advances_even_when_nothing_was_fired() {
        // skip 策略丢弃了积压，但游标必须前进，否则下个 tick 会重算同一批
        let cron = CronSchedule::parse("*/1 * * * *", "UTC").expect("解析");
        let newest = Some(at("2026-09-08T10:03:00Z"));
        let position = next_position(&cron, &schedule(0), &newest, at("2026-09-08T10:03:20Z"));
        assert_eq!(position.last_fired_at, newest);
    }

    #[test]
    fn jitter_delays_the_claim_but_never_the_canonical_fire_point() {
        let cron = CronSchedule::parse("0 * * * *", "UTC").expect("解析");
        let now = at("2026-09-08T10:00:05Z");
        for _ in 0..100 {
            let position =
                next_position(&cron, &schedule(30), &Some(at("2026-09-08T10:00:00Z")), now);
            // 规范触发点必须是整点，不能被抖动挪动——它要写进 runs.fire_at，
            // 而 UNIQUE(schedule_id, fire_at) 是定时幂等的全部依靠
            assert_eq!(position.next_fire_at, at("2026-09-08T11:00:00Z"));
            let delay = (position.next_claim_at - position.next_fire_at).num_seconds();
            assert!((0..=30).contains(&delay), "抖动 {delay}s 越界");
        }
    }

    #[test]
    fn zero_jitter_makes_claim_and_fire_identical() {
        let cron = CronSchedule::parse("0 * * * *", "UTC").expect("解析");
        let position = next_position(
            &cron,
            &schedule(0),
            &Some(at("2026-09-08T10:00:00Z")),
            at("2026-09-08T10:00:05Z"),
        );
        assert_eq!(position.next_claim_at, position.next_fire_at);
    }

    #[test]
    fn grace_is_long_enough_for_a_slow_tick_but_short_enough_to_catch_an_outage() {
        // 轮询间隔是秒级，宽限期要能吸收正常抖动；
        // 又不能长到把"停机几分钟"也当成准点
        assert!(GRACE >= chrono::Duration::seconds(5));
        assert!(GRACE <= chrono::Duration::minutes(2));
    }
}
