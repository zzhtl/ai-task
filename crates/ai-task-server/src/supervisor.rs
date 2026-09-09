//! 在跑的 run 的登记处。
//!
//! 只做一件事：让 `POST /runs/{id}/cancel` 能找到对应的取消令牌。
//! 调度、并发限流是 M2 的事。

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
}

impl RunSupervisor {
    #[must_use]
    pub fn new(engine: RunEngine) -> Self {
        Self {
            engine,
            running: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 后台启动一个 run。立刻返回，不等它跑完。
    pub fn spawn(&self, workspace_id: WorkspaceId, run_id: RunId) {
        let cancel = CancellationToken::new();
        self.register(run_id, cancel.clone());

        let engine = self.engine.clone();
        let running = Arc::clone(&self.running);
        tokio::spawn(async move {
            engine.execute(workspace_id, run_id, cancel).await;
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
