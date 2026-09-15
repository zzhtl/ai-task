//! 事件日志的读写。
//!
//! 写入路径是整个系统的热点，两条约束：
//!
//! 1. **seq 由数据库分配。** `UPDATE runs SET max_seq = max_seq + n RETURNING max_seq`
//!    在事务里拿到行锁，因此即使出现两个写入者（引擎重启时新旧任务短暂并存），
//!    也不会撞号。不依赖「单写者」这个不变量成立。
//! 2. **批量写。** 一次执行会刷出几百条 `agent_text`，一条一个事务会把
//!    数据库打满。API 直接收一批，调用方决定攒多久。

use ai_task_proto::{RunEvent, RunEventBody, RunId};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Row, Transaction};

use crate::{Store, StoreError};

/// Postgres `NOTIFY` 用的频道名。
///
/// 负载只带 `run_id:max_seq` 这样一个**唤醒提示**，真值让订阅方回库里读：
/// NOTIFY 的负载有 8000 字节上限，而且在某些边界下并不保证送达。
pub const EVENTS_CHANNEL: &str = "ai_task_run_events";

/// 待写入的一条事件（还没有 seq，seq 由库分配）。
#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub node_key: Option<String>,
    pub body: RunEventBody,
}

impl PendingEvent {
    #[must_use]
    pub fn new(node_key: Option<String>, body: RunEventBody) -> Self {
        Self { node_key, body }
    }

    /// run 级事件，不归属任何节点。
    #[must_use]
    pub fn run(body: RunEventBody) -> Self {
        Self::new(None, body)
    }
}

impl Store {
    /// 追加一批事件，返回分配到的 seq 区间的末值。
    ///
    /// 空批次是合法的（调用方的定时刷新可能什么都没攒到），直接返回当前 max_seq。
    pub async fn append_events(
        &self,
        run_id: RunId,
        events: &[PendingEvent],
    ) -> Result<i64, StoreError> {
        let mut tx = self.pool().begin().await?;
        let last_seq = append_in_tx(&mut tx, run_id, events).await?;
        tx.commit().await?;

        if !events.is_empty() {
            self.notify_events(run_id, last_seq).await?;
        }
        Ok(last_seq)
    }

    /// 通知其它副本：这个 run 有新事件了。
    ///
    /// 失败只记日志不报错——通知丢了最多是订阅方晚一点（它们有轮询兜底），
    /// 但事件已经落库了，不该因此让写入路径失败。
    async fn notify_events(&self, run_id: RunId, max_seq: i64) -> Result<(), StoreError> {
        let payload = format!("{run_id}:{max_seq}");
        let result = sqlx::query("SELECT pg_notify($1, $2)")
            .bind(EVENTS_CHANNEL)
            .bind(&payload)
            .execute(self.pool())
            .await;
        if let Err(err) = result {
            tracing::warn!(error = %err, %run_id, "pg_notify 失败，订阅方会靠轮询兜底");
        }
        Ok(())
    }

    /// 读取 `after_seq` 之后的事件，最多 `limit` 条。
    ///
    /// SSE 断线续传就是拿 `Last-Event-ID` 当 `after_seq` 调这个。
    /// 续传读。
    ///
    /// `not_before` 是**分区裁剪**用的：`run_events` 按 ts 月度分区，
    /// 而这条查询只有 run_id 和 seq 两个条件，于是 planner 只能 Merge Append
    /// 扫过**所有**分区——分区数每月 +1，这条 SSE 热路径就越走越慢。
    /// 调用方手上有这个 run 的 created_at，给一个下界就能裁掉所有更早的分区。
    /// 传 `None` 表示不裁（回放整条流时用）。
    pub async fn read_events_after(
        &self,
        run_id: RunId,
        after_seq: i64,
        limit: i64,
        not_before: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<RunEvent>, StoreError> {
        let rows = sqlx::query(
            "SELECT seq, ts, node_key, payload
             FROM run_events
             WHERE run_id = $1 AND seq > $2
               AND ($4::timestamptz IS NULL OR ts >= $4)
             ORDER BY seq
             LIMIT $3",
        )
        .bind(uuid::Uuid::from(run_id))
        .bind(after_seq)
        .bind(limit)
        .bind(not_before)
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                let payload: serde_json::Value = row.try_get("payload")?;
                let body: RunEventBody = serde_json::from_value(payload).map_err(|err| {
                    // 历史事件解不出来说明 RunEventBody 出现了破坏性变更。
                    // 这是设计错误，不是运行时意外——必须显式炸出来，
                    // 不能悄悄跳过让回放出一个错误的状态。
                    StoreError::Corrupt {
                        what: "run_events.payload",
                        detail: err.to_string(),
                    }
                })?;
                Ok(RunEvent {
                    run_id,
                    seq: row.try_get("seq")?,
                    ts: row.try_get::<DateTime<Utc>, _>("ts")?,
                    node_key: row
                        .try_get::<Option<String>, _>("node_key")?
                        .map(ai_task_proto::NodeKey),
                    body,
                })
            })
            .collect()
    }
}

/// 在已有事务里追加事件。供需要「状态变更和事件写入同一个事务」的调用方使用。
pub async fn append_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    run_id: RunId,
    events: &[PendingEvent],
) -> Result<i64, StoreError> {
    // 拿行锁并一次性预留 n 个 seq。并发写入者会在这里排队，不会撞号。
    let last_seq: i64 = sqlx::query_scalar(
        "UPDATE runs SET max_seq = max_seq + $2 WHERE id = $1 RETURNING max_seq",
    )
    .bind(uuid::Uuid::from(run_id))
    .bind(i64::try_from(events.len()).unwrap_or(i64::MAX))
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(StoreError::NotFound {
        what: "run",
        id: run_id.to_string(),
    })?;

    if events.is_empty() {
        return Ok(last_seq);
    }

    let first_seq = last_seq - i64::try_from(events.len()).unwrap_or(0) + 1;
    let mut seqs = Vec::with_capacity(events.len());
    let mut node_keys = Vec::with_capacity(events.len());
    let mut kinds = Vec::with_capacity(events.len());
    let mut payloads = Vec::with_capacity(events.len());
    for (offset, event) in events.iter().enumerate() {
        seqs.push(first_seq + i64::try_from(offset).unwrap_or(0));
        node_keys.push(event.node_key.clone());
        kinds.push(event.body.kind().to_string());
        payloads.push(
            serde_json::to_value(&event.body).map_err(|err| StoreError::Corrupt {
                what: "RunEventBody",
                detail: err.to_string(),
            })?,
        );
    }

    // UNNEST 批量插入：一次往返写完整批，而不是 n 次往返
    sqlx::query(
        "INSERT INTO run_events (run_id, seq, ts, node_key, kind, payload)
         SELECT $1, s, now(), n, k, p
         FROM UNNEST($2::bigint[], $3::text[], $4::text[], $5::jsonb[]) AS t(s, n, k, p)",
    )
    .bind(uuid::Uuid::from(run_id))
    .bind(&seqs)
    .bind(&node_keys)
    .bind(&kinds)
    .bind(&payloads)
    .execute(&mut **tx)
    .await?;

    Ok(last_seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_payload_is_a_hint_not_the_data() {
        // NOTIFY 负载有 8000 字节上限且不保证送达，所以只送 run_id:seq，
        // 真数据让订阅方回库里读
        let run_id = RunId::new();
        let payload = format!("{run_id}:{}", i64::MAX);
        assert!(payload.len() < 64, "唤醒提示必须很小：{payload}");
    }

    #[test]
    fn run_level_events_carry_no_node_key() {
        let e = PendingEvent::run(RunEventBody::NodeReady);
        assert!(e.node_key.is_none());
    }
}
