//! 在跑的 run 的登记处，外加一道并发闸。
//!
//! 每个 run 会拉起一个 `claude` 子进程——吃 CPU、吃内存、**花钱**。
//! 这里原本是无上限的 `tokio::spawn`，一次触发风暴或者 cron 的 `fire_all` 补偿
//! 就能把子进程数顶到没边。
//!
//! 闸门用信号量，**permit 在 spawn 出来的任务内部获取**：这样超额的 run
//! 在拿到许可之前一直停在 `queued`——那正是现有状态机里已有的语义，
//! 不需要为"排队"新造一个状态。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ai_task_proto::{RunId, WorkspaceId};
use ai_task_runtime::RunEngine;
use tokio_util::sync::CancellationToken;

/// 在跑的 run 及其取消令牌。
#[derive(Clone)]
pub struct RunSupervisor {
    engine: RunEngine,
    // Mutex 而不是 RwLock：临界区只是一次 HashMap 增删，
    // RwLock 的读写分离在这个粒度上只会更慢。
    running: Arc<Mutex<HashMap<RunId, CancellationToken>>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl RunSupervisor {
    #[must_use]
    pub fn new(engine: RunEngine, max_concurrent: usize) -> Self {
        tracing::info!(max_concurrent, "run 并发上限");
        Self {
            engine,
            running: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
        }
    }

    /// 后台启动一个 run。立刻返回，不等它跑完。
    pub fn spawn(&self, workspace_id: WorkspaceId, run_id: RunId) {
        let cancel = CancellationToken::new();
        self.register(run_id, cancel.clone());

        let engine = self.engine.clone();
        let running = Arc::clone(&self.running);
        let slots = Arc::clone(&self.slots);
        tokio::spawn(async move {
            // 等一个空位。**等的时候也要能取消**——不然队列很长时，
            // 用户点了取消却要等前面几十个跑完才生效，看起来就是没反应。
            let permit = tokio::select! {
                () = cancel.cancelled() => None,
                permit = slots.acquire_owned() => permit.ok(),
            };

            if let Some(permit) = permit {
                engine.execute(workspace_id, run_id, cancel).await;
                drop(permit);
            } else {
                tracing::info!(%run_id, "排队期间被取消，没有启动");
            }

            // 无论结局如何都要摘掉登记，否则这个表会一直涨
            if let Ok(mut map) = running.lock() {
                map.remove(&run_id);
            }
        });
    }

    /// 请求取消。返回 `false` 表示这个 run 不在本副本上跑。
    ///
    /// M1 只能取消本副本的 run。跨副本取消要走数据库标记位，是 M2 的事。
    #[must_use]
    pub fn cancel(&self, run_id: RunId) -> bool {
        let Ok(map) = self.running.lock() else {
            return false;
        };
        match map.get(&run_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    fn register(&self, run_id: RunId, cancel: CancellationToken) {
        if let Ok(mut map) = self.running.lock() {
            map.insert(run_id, cancel);
        }
    }
}
