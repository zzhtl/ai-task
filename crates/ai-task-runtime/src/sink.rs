//! 事件写入。
//!
//! 一次 AI 执行会刷出几百条 `agent_text`。一条一个事务能把数据库打满，
//! 但攒太久前端就不"实时"了。这里用**双触发攒批**：满 [`MAX_BATCH`] 条
//! 或者过了 [`FLUSH_INTERVAL`] 就写一次，两者取先到的。

use std::time::Duration;

use ai_task_proto::{NodeKey, RunEventBody, RunId};
use ai_task_store::{PendingEvent, Store, StoreError};
use tokio_util::sync::CancellationToken;

/// 攒批上限。再多就该落库了，免得进程崩了丢一大段。
const MAX_BATCH: usize = 32;

/// 强制刷新间隔。
///
/// 必须明显小于「用户觉得卡了」的阈值：M1 的验收要求点击后 1 秒内能看到
/// 流式事件，200ms 给网络和渲染留了足够余量。
const FLUSH_INTERVAL: Duration = Duration::from_millis(200);

/// 事件写入的抽象。
///
/// 抽成 trait 是为了让 DAG 编排能在**不碰数据库**的前提下测试——条件边、重试、
/// map 展开这些逻辑的 bug，不该靠起容器才能发现；顺带还能在测试里直接断言
/// 事件序列。
#[async_trait::async_trait]
pub trait EventWriter: Send {
    /// 记一条节点事件。
    async fn node(&mut self, node: &NodeKey, body: RunEventBody) -> Result<(), StoreError>;

    /// 记一条 run 级事件（不归属任何节点）。
    async fn run(&mut self, body: RunEventBody) -> Result<(), StoreError>;
}

#[async_trait::async_trait]
impl EventWriter for EventSink {
    async fn node(&mut self, node: &NodeKey, body: RunEventBody) -> Result<(), StoreError> {
        EventSink::node(self, node, body).await
    }

    async fn run(&mut self, body: RunEventBody) -> Result<(), StoreError> {
        EventSink::run(self, body).await
    }
}

/// 带攒批的事件写入器。
pub struct EventSink {
    store: Store,
    run_id: RunId,
    buffer: Vec<PendingEvent>,
    last_flush: tokio::time::Instant,
}

/// 缓冲里最多留多少条没落库的事件。
///
/// 只有数据库持续不可用时才会堆到这里。超过就开始丢最早的：
/// 到那个份上，保住进程比保住事件更要紧。
const MAX_PENDING: usize = 10_000;

impl EventSink {
    #[must_use]
    pub fn new(store: Store, run_id: RunId) -> Self {
        Self {
            store,
            run_id,
            buffer: Vec::with_capacity(MAX_BATCH),
            last_flush: tokio::time::Instant::now(),
        }
    }

    /// 记一条节点事件。到阈值会自动落库。
    pub async fn node(&mut self, node: &NodeKey, body: RunEventBody) -> Result<(), StoreError> {
        self.push(PendingEvent::new(Some(node.to_string()), body))
            .await
    }

    /// 记一条 run 级事件。
    pub async fn run(&mut self, body: RunEventBody) -> Result<(), StoreError> {
        self.push(PendingEvent::run(body)).await
    }

    async fn push(&mut self, event: PendingEvent) -> Result<(), StoreError> {
        self.buffer.push(event);
        if self.should_flush() {
            self.flush().await?;
        }
        Ok(())
    }

    fn should_flush(&self) -> bool {
        self.buffer.len() >= MAX_BATCH || self.last_flush.elapsed() >= FLUSH_INTERVAL
    }

    /// 立刻落库。缓冲为空时是空操作。
    pub async fn flush(&mut self) -> Result<(), StoreError> {
        if self.buffer.is_empty() {
            self.last_flush = tokio::time::Instant::now();
            return Ok(());
        }
        // **写失败要把这一批留着。**
        //
        // 这里原本是"先清空再写"，注释说丢掉的只是展示用的增量。那句话不对：
        // `NodeFinished`、`PolicyDecided`、`ToolRequested` 都从这条路走
        // （见 dag.rs 的 finish_node 和 engine.rs 的事件翻译），
        // 它们既是状态推导的依据，也是"这次工具调用被拦下来了吗"的审计材料。
        // 在一个事件溯源的系统里安静地丢掉一批，等于让回放得出另一个结论。
        //
        // 留着的代价是内存，所以有个硬上限兜底：真堆到那个份上，
        // 数据库已经长时间不可用，这时候保住进程比保住事件更要紧。
        let batch = std::mem::take(&mut self.buffer);
        self.last_flush = tokio::time::Instant::now();
        if let Err(err) = self.store.append_events(self.run_id, &batch).await {
            let restored = self.buffer.len() + batch.len();
            if restored <= MAX_PENDING {
                // 放回队首：事件之间有顺序，新来的不能插到失败那批前面
                let mut merged = batch;
                merged.append(&mut self.buffer);
                self.buffer = merged;
                tracing::warn!(
                    run_id = %self.run_id,
                    pending = self.buffer.len(),
                    error = %err,
                    "事件落库失败，留在缓冲里等下一次"
                );
            } else {
                tracing::error!(
                    run_id = %self.run_id,
                    dropped = batch.len(),
                    error = %err,
                    "事件缓冲超过上限，丢弃最早的一批——这个 run 的回放会不完整"
                );
            }
            return Err(err);
        }
        Ok(())
    }

    /// 距离下次强制刷新还有多久。[`flush_on_interval`] 用它定下一次醒来的时间。
    #[must_use]
    pub fn until_next_flush(&self) -> Duration {
        FLUSH_INTERVAL.saturating_sub(self.last_flush.elapsed())
    }

    /// 缓冲里还有多少条没落库。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}

/// 执行期间按间隔刷新，直到 `stop` 被取消。
///
/// [`EventSink::push`] 只在下一条事件进来时才看间隔。审批门在等人、shell 在跑
/// 长命令的时候没有下一条，已经缓冲的事件（上一步的 `NodeFinished`、这一步的
/// `NodeStarted`）会一直压着，界面上上一步就卡在"执行中"。
///
/// 停止信号只在两次等待之间检查，**不会在 `flush` 半途被打断**：`flush` 先把
/// 整批从缓冲里取走再写库，中途丢掉这个 future 就是丢事件。
pub async fn flush_on_interval(sink: &tokio::sync::Mutex<EventSink>, stop: &CancellationToken) {
    loop {
        let wait = sink.lock().await.until_next_flush();
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
        let mut sink = sink.lock().await;
        // 等待期间事件自己触发过刷新的话，时间还没到，接着等
        if sink.until_next_flush().is_zero() {
            // 失败时这批留在缓冲里、sink 已经记了日志，下一轮或下一条事件会再试
            let _ = sink.flush().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_interval_leaves_room_for_the_one_second_budget() {
        // M1 的验收：点击后 1 秒内前端要出现流式事件。
        // 刷新间隔要给网络往返和渲染留出余量。
        assert!(
            FLUSH_INTERVAL <= Duration::from_millis(300),
            "刷新间隔 {FLUSH_INTERVAL:?} 太长，撑不起 1 秒内可见"
        );
        assert!(
            FLUSH_INTERVAL >= Duration::from_millis(50),
            "太短会退化成一条一个事务"
        );
    }

    #[test]
    fn batch_cap_is_bounded() {
        // 进程崩溃最多丢一批，所以这个数不能太大
        assert!((8..=128).contains(&MAX_BATCH));
    }
}
