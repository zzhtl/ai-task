//! 等一个人点头。
//!
//! `approval` 节点和策略判决 `effect: ask` 都走这里。等待方式是轮询数据库，
//! 不是内存里的 channel：决策来自另一个 HTTP 请求，可能落在**另一个副本**上，
//! 进程内的等待者根本收不到。数据库是唯一的真相。
//!
//! 轮询频率 2 秒。默认 15 分钟的窗口最多 450 次单行主键查询，
//! 相比"人正坐在那里等"的场景，这点开销不值得用 LISTEN/NOTIFY 换。

use std::time::Duration;

use ai_task_proto::{ApprovalId, RunId};
use ai_task_store::{NewApproval, Store};
use tokio_util::sync::CancellationToken;

/// 轮询间隔。
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 一次审批的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approved {
        by: Option<String>,
        reason: Option<String>,
    },
    Denied {
        by: Option<String>,
        reason: Option<String>,
    },
    /// 到点没人决策。**默认动作是拒绝**，这里只是把"为什么"分开。
    TimedOut,
    /// run 被取消了，别再等了。
    Cancelled,
}

impl Verdict {
    #[must_use]
    pub fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }

    /// 回给模型/节点的原因。
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Approved { by, .. } => match by {
                Some(who) => format!("已由 {who} 批准"),
                None => "已批准".to_owned(),
            },
            Self::Denied { by, reason } => {
                let who = by.as_deref().unwrap_or("审批人");
                match reason {
                    Some(why) if !why.trim().is_empty() => format!("{who} 拒绝：{why}"),
                    _ => format!("{who} 拒绝了这次操作"),
                }
            }
            Self::TimedOut => "等待审批超时，按拒绝处理".to_owned(),
            Self::Cancelled => "run 已取消，审批作废".to_owned(),
        }
    }
}

/// 建一条审批并等它有结论。
///
/// 返回 `(approval_id, verdict)`：调用方需要 id 来写事件。
pub async fn request_and_wait(
    store: &Store,
    new: NewApproval,
    cancel: &CancellationToken,
) -> Result<(ApprovalId, Verdict), ai_task_store::StoreError> {
    let expires_at = new.expires_at;
    let run_id = new.run_id;
    let id = store.create_approval(new).await?;
    let verdict = wait(store, id, run_id, expires_at, cancel).await?;
    Ok((id, verdict))
}

/// 等一条已经建好的审批。过期时间从库里读。
pub async fn wait_for_verdict(
    store: &Store,
    id: ApprovalId,
    cancel: &CancellationToken,
) -> Result<Verdict, ai_task_store::StoreError> {
    let Some((run_id, expires_at)) = store.approval_deadline(id).await? else {
        // 审批不见了（run 被删）。没有结论就是没批准。
        return Ok(Verdict::Denied {
            by: None,
            reason: Some("审批记录已不存在".into()),
        });
    };
    wait(store, id, run_id, expires_at, cancel).await
}

async fn wait(
    store: &Store,
    id: ApprovalId,
    run_id: RunId,
    expires_at: chrono::DateTime<chrono::Utc>,
    cancel: &CancellationToken,
) -> Result<Verdict, ai_task_store::StoreError> {
    loop {
        // 先查一次再睡：决策可能在建完到第一次轮询之间就到了
        if let Some((approved, reason, by)) = store.approval_verdict(id).await? {
            let by = match by {
                Some(user) => store.user_display_name(user).await.unwrap_or(None),
                None => None,
            };
            return Ok(if approved {
                Verdict::Approved { by, reason }
            } else {
                Verdict::Denied { by, reason }
            });
        }

        if chrono::Utc::now() >= expires_at {
            // 落库之后再返回。只在内存里当作超时的话，
            // 界面上那张卡片会永远停在"待审批"。
            store.expire_approvals(chrono::Utc::now()).await?;
            return Ok(Verdict::TimedOut);
        }

        // run 被取消时不该继续占着一个 worker 等 15 分钟
        tokio::select! {
            () = cancel.cancelled() => {
                store
                    .decide_approval(
                        store.workspace_of_run(run_id).await?,
                        id,
                        false,
                        None,
                        Some("run 已取消"),
                    )
                    .await?;
                return Ok(Verdict::Cancelled);
            }
            () = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_is_reported_as_a_refusal_not_as_an_error() {
        // 超时必须是"拒绝"，不是"出错了"。后者会让人以为重试一下就好，
        // 而审批门的意义正是"没人点头就不做"。
        assert!(!Verdict::TimedOut.is_approved());
        assert!(Verdict::TimedOut.describe().contains("拒绝"));
    }

    #[test]
    fn a_denial_carries_who_and_why_so_the_model_can_adjust() {
        let verdict = Verdict::Denied {
            by: Some("zzh".into()),
            reason: Some("这台是主库，别动".into()),
        };
        let text = verdict.describe();
        assert!(text.contains("zzh"), "{text}");
        assert!(text.contains("主库"), "{text}");
    }

    #[test]
    fn a_denial_without_a_reason_still_reads_as_a_sentence() {
        let text = Verdict::Denied {
            by: None,
            reason: Some("   ".into()),
        }
        .describe();
        assert!(!text.contains("：  "), "空白理由不该拼进去：{text}");
        assert!(text.contains("拒绝"), "{text}");
    }
}
