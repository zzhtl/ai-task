//! `Idempotency-Key` 的存储侧。
//!
//! ADR 0002 要求：**响应与副作用写在同一个事务里**，重放返回存下来的响应，
//! 同 key 不同 body 返回 409，窗口 ≥24h。
//!
//! 表、DTO 文档和前端请求头一直都在，**服务端一行代码都没有**——
//! 于是双击一次「运行」就产生两个 run，而界面上写着这个接口是幂等的。
//!
//! 做成"在 handler 的事务里调用的助手"而不是 tower layer：layer 进不到 handler
//! 的事务里去，而"同一个事务"正是这套东西唯一有意义的地方。

use chrono::Utc;
use sqlx::{Postgres, Row, Transaction};

use crate::pool::StoreError;
use ai_task_proto::WorkspaceId;

/// 窗口。ADR 0002 要求至少 24 小时。
const TTL_HOURS: i64 = 48;

/// 占位行用的状态码。表示"这次请求正在处理中，还没有结果"。
const IN_FLIGHT: i32 = 0;

/// 拿这个 key 开工的结果。
#[derive(Debug)]
pub enum Claim {
    /// 头一次见到这个 key，继续干活。
    Fresh,
    /// 之前干完了。把存下来的响应原样返回。
    Replay {
        status: i32,
        body: serde_json::Value,
    },
    /// 同一个 key 配了不一样的 body。
    Conflict,
    /// 同一个 key 的上一次还没干完。
    InFlight,
}

/// 在事务里认领一个幂等键。
///
/// `INSERT ... ON CONFLICT DO NOTHING RETURNING` 让"抢到"这件事本身是原子的：
/// 并发的两次同 key 请求里只有一次能插进去，另一次读到占位行。
pub async fn claim(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: WorkspaceId,
    key: &str,
    request_hash: &str,
) -> Result<Claim, StoreError> {
    let inserted = sqlx::query(
        "INSERT INTO idempotency_keys
             (workspace_id, key, request_hash, status_code, response, expires_at)
         VALUES ($1, $2, $3, $4, 'null'::jsonb, now() + make_interval(hours => $5))
         ON CONFLICT (workspace_id, key) DO NOTHING
         RETURNING 1 AS ok",
    )
    .bind(uuid::Uuid::from(workspace_id))
    .bind(key)
    .bind(request_hash)
    .bind(IN_FLIGHT)
    .bind(i32::try_from(TTL_HOURS).unwrap_or(48))
    .fetch_optional(&mut **tx)
    .await?;

    if inserted.is_some() {
        return Ok(Claim::Fresh);
    }

    let row = sqlx::query(
        "SELECT request_hash, status_code, response, expires_at
         FROM idempotency_keys WHERE workspace_id = $1 AND key = $2",
    )
    .bind(uuid::Uuid::from(workspace_id))
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(row) = row else {
        // 插不进去又读不到：只可能是并发的清理刚好删掉了过期行。当成新的。
        return Ok(Claim::Fresh);
    };

    // 过期的当成没见过：把它换成新的占位行
    let expires_at: chrono::DateTime<Utc> = row.try_get("expires_at")?;
    if expires_at <= Utc::now() {
        sqlx::query(
            "UPDATE idempotency_keys
             SET request_hash = $3, status_code = $4, response = 'null'::jsonb,
                 created_at = now(), expires_at = now() + make_interval(hours => $5)
             WHERE workspace_id = $1 AND key = $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(key)
        .bind(request_hash)
        .bind(IN_FLIGHT)
        .bind(i32::try_from(TTL_HOURS).unwrap_or(48))
        .execute(&mut **tx)
        .await?;
        return Ok(Claim::Fresh);
    }

    let stored_hash: String = row.try_get("request_hash")?;
    if stored_hash != request_hash {
        // 同一个 key 配了不一样的 body。返回旧结果等于悄悄丢掉这次请求。
        return Ok(Claim::Conflict);
    }

    let status: i32 = row.try_get("status_code")?;
    if status == IN_FLIGHT {
        // 上一次还在飞。这里返回 409 而不是阻塞等锁：
        // 等锁会把连接占住，而客户端拿到 409 之后自己重试更便宜。
        return Ok(Claim::InFlight);
    }

    Ok(Claim::Replay {
        status,
        body: row.try_get("response")?,
    })
}

/// 把结果写回占位行。**必须和副作用在同一个事务里提交。**
pub async fn finish(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: WorkspaceId,
    key: &str,
    status: i32,
    body: &serde_json::Value,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE idempotency_keys SET status_code = $3, response = $4
         WHERE workspace_id = $1 AND key = $2",
    )
    .bind(uuid::Uuid::from(workspace_id))
    .bind(key)
    .bind(status)
    .bind(body)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 放弃这个 key。
///
/// 只持久化 2xx：4xx 时把占位行删掉，否则客户端把 body 改对了再用同一个 key 重试，
/// 拿回来的永远是那条旧错误。ADR 0002 没规定这一条，是判断。
pub async fn release(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: WorkspaceId,
    key: &str,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM idempotency_keys WHERE workspace_id = $1 AND key = $2")
        .bind(uuid::Uuid::from(workspace_id))
        .bind(key)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// 清理过期的键。维护任务每轮调一次。
pub async fn purge_expired(pool: &sqlx::PgPool) -> Result<u64, StoreError> {
    let done = sqlx::query("DELETE FROM idempotency_keys WHERE expires_at <= now()")
        .execute(pool)
        .await?;
    Ok(done.rows_affected())
}

/// 一次幂等创建的身份信息。
///
/// 收成一个结构体而不是四个平铺的参数：它们总是一起出现，
/// 而且 `key` 和 `request_hash` 挨着传的时候很容易写反。
pub struct IdempotentCreate<'a> {
    pub workspace_id: WorkspaceId,
    /// 客户端给的键。`None` 表示不做去重。
    pub key: Option<&'a str>,
    /// 请求原始字节的哈希。
    pub request_hash: &'a str,
    /// 成功时要存下来的状态码。
    pub status: i32,
}

/// 幂等地建一个 run。
///
/// 整套东西收在 store 里而不是让 handler 自己开事务：
/// `docs/architecture.md` 定死了「整个仓库只有 ai-task-store 写 SQL」，
/// 把 `sqlx::Transaction` 递到 server crate 去就破了那条线。
pub enum IdempotentRun {
    /// 新建的。装箱是因为 `RunRecord` 比其它几个变体大一个数量级，
    /// 不装箱的话每次返回都要搬那么多字节，而重放路径根本用不上。
    Created(Box<crate::RunRecord>),
    /// 同 key 同 body 的重放。
    Replayed {
        status: i32,
        body: serde_json::Value,
    },
    /// 同 key 不同 body。
    Conflict,
    /// 同 key 的上一次还在处理。
    InFlight,
}

impl crate::Store {
    /// 建 run + 写幂等键，**一个事务**。
    ///
    /// `render` 把建好的 run 变成要存下来的响应体；它在事务内调用，
    /// 所以重放时返回的和第一次返回的是同一份内容。
    pub async fn create_run_idempotent(
        &self,
        request: IdempotentCreate<'_>,
        new: crate::NewRun,
        queued_event: crate::PendingEvent,
        render: impl FnOnce(&crate::RunRecord) -> serde_json::Value,
    ) -> Result<IdempotentRun, StoreError> {
        let IdempotentCreate {
            workspace_id,
            key,
            request_hash,
            status,
        } = request;
        let mut tx = self.pool().begin().await?;

        if let Some(key) = key {
            match claim(&mut tx, workspace_id, key, request_hash).await? {
                Claim::Replay { status, body } => {
                    tx.commit().await?;
                    return Ok(IdempotentRun::Replayed { status, body });
                }
                Claim::Conflict => {
                    tx.commit().await?;
                    return Ok(IdempotentRun::Conflict);
                }
                Claim::InFlight => {
                    tx.commit().await?;
                    return Ok(IdempotentRun::InFlight);
                }
                Claim::Fresh => {}
            }
        }

        let run = crate::Store::create_run_in_tx(&mut tx, new, queued_event).await?;
        if let Some(key) = key {
            finish(&mut tx, workspace_id, key, status, &render(&run)).await?;
        }
        tx.commit().await?;

        // 事务里发不了通知，提交之后补上——否则 SSE 订阅者要等兜底轮询才看得见第一条事件
        self.notify_run(run.id, 1).await;
        Ok(IdempotentRun::Created(Box::new(run)))
    }
}
