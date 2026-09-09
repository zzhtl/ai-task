//! 事件写入。
//!
//! 一次 AI 执行会刷出几百条 `agent_text`。一条一个事务能把数据库打满，
//! 但攒太久前端就不"实时"了。这里用**双触发攒批**：满 [`MAX_BATCH`] 条
//! 或者过了 [`FLUSH_INTERVAL`] 就写一次，两者取先到的。

use std::time::Duration;

use ai_task_proto::{NodeKey, RunEventBody, RunId};
use ai_task_store::{PendingEvent, Store, StoreError};

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
        // 先清空缓冲再写：写失败时缓冲已经腾空，不会在重试里越堆越大。
        // 代价是那一批事件丢了——但它们是展示用的增量，run 的状态推导
        // 依赖的是状态类事件，那些走 store 的事务接口单独写。
        let batch = std::mem::take(&mut self.buffer);
        self.last_flush = tokio::time::Instant::now();
        self.store.append_events(self.run_id, &batch).await?;
        Ok(())
    }

    /// 距离下次强制刷新还有多久。执行循环用它设 select 的定时器。
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
