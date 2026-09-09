//! 事件唤醒总线。
//!
//! SSE 连接不轮询数据库，而是等一个**唤醒提示**再去读增量。提示的来源有两个：
//!
//! - 本副本自己写事件后直接发（同进程，零延迟）
//! - Postgres `LISTEN`/`NOTIFY`（跨副本）
//!
//! 提示只带 `(run_id, max_seq)`，**真值一律回库里读**：NOTIFY 的负载有 8000
//! 字节上限，而且并不保证送达。丢一次提示只意味着晚一点——兜底轮询会兜住。

use ai_task_proto::RunId;
use ai_task_store::{EVENTS_CHANNEL, Store};
use tokio::sync::broadcast;

/// 广播通道容量。
///
/// 订阅者落后太多会收到 `Lagged`，那时它只要按自己的 last_seq 回库里补读即可
/// ——提示丢了不影响正确性，所以这个数不用很大。
const BUS_CAPACITY: usize = 1024;

/// 一次唤醒提示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wakeup {
    pub run_id: RunId,
    pub max_seq: i64,
}

/// 进程内的唤醒广播。
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Wakeup>,
}

impl EventBus {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BUS_CAPACITY);
        Self { tx }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Wakeup> {
        self.tx.subscribe()
    }

    /// 发一个提示。没有订阅者时是空操作，不算错误。
    pub fn publish(&self, wakeup: Wakeup) {
        let _ = self.tx.send(wakeup);
    }

    /// 起一个后台任务，把 Postgres 的通知转成进程内广播。
    ///
    /// 连接断了会自己重连——`PgListener` 负责这件事。重连期间丢掉的提示由
    /// SSE 端的兜底轮询补上。
    pub fn spawn_pg_listener(&self, store: Store) -> tokio::task::JoinHandle<()> {
        let bus = self.clone();
        tokio::spawn(async move {
            loop {
                match sqlx::postgres::PgListener::connect_with(store.pool()).await {
                    Ok(mut listener) => {
                        if let Err(err) = listener.listen(EVENTS_CHANNEL).await {
                            tracing::warn!(error = %err, "LISTEN 失败，稍后重试");
                        } else {
                            tracing::info!(channel = EVENTS_CHANNEL, "已订阅事件通知");
                            while let Ok(notification) = listener.recv().await {
                                match parse_wakeup(notification.payload()) {
                                    Some(wakeup) => bus.publish(wakeup),
                                    None => tracing::warn!(
                                        payload = notification.payload(),
                                        "无法解析的通知负载"
                                    ),
                                }
                            }
                        }
                    }
                    Err(err) => tracing::warn!(error = %err, "订阅事件通知失败，稍后重试"),
                }
                // 连接断了就退避重连。SSE 端有兜底轮询，这里不用抢。
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        })
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 `<run_id>:<max_seq>` 形式的通知负载。
fn parse_wakeup(payload: &str) -> Option<Wakeup> {
    let (run, seq) = payload.split_once(':')?;
    Some(Wakeup {
        run_id: RunId(run.parse().ok()?),
        max_seq: seq.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakeup_payload_round_trips() {
        let run_id = RunId::new();
        let parsed = parse_wakeup(&format!("{run_id}:42")).expect("应当解析成功");
        assert_eq!(parsed.run_id, run_id);
        assert_eq!(parsed.max_seq, 42);
    }

    #[test]
    fn malformed_payloads_are_rejected_not_guessed() {
        // 解析失败只记日志；兜底轮询会保证事件最终送达
        assert!(parse_wakeup("").is_none());
        assert!(parse_wakeup("no-colon").is_none());
        assert!(parse_wakeup("not-a-uuid:1").is_none());
        assert!(parse_wakeup(&format!("{}:abc", RunId::new())).is_none());
    }

    #[tokio::test]
    async fn publishing_without_subscribers_is_not_an_error() {
        let bus = EventBus::new();
        bus.publish(Wakeup {
            run_id: RunId::new(),
            max_seq: 1,
        });
    }

    #[tokio::test]
    async fn subscribers_receive_wakeups() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let wakeup = Wakeup {
            run_id: RunId::new(),
            max_seq: 7,
        };
        bus.publish(wakeup);
        assert_eq!(rx.recv().await.expect("应当收到"), wakeup);
    }
}
