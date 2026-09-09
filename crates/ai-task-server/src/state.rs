//! 进程级共享状态。

use std::sync::Arc;

use ai_task_proto::WorkspaceId;
use ai_task_runtime::RunEngine;

use crate::bus::EventBus;
use crate::supervisor::RunSupervisor;

/// axum handler 拿到的状态。克隆代价等同克隆一个 `Arc`。
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub store: ai_task_store::Store,
    pub bus: EventBus,
    pub supervisor: RunSupervisor,
    /// 当前 workspace。
    ///
    /// M1 没有认证，全进程共用一个。**但每条查询都带着它**——等 M5 接入认证时，
    /// 换成从会话里取即可，不用回头去给几十条 SQL 补租户条件。
    pub workspace_id: WorkspaceId,
    /// 内部接口（策略判决）的令牌。
    ///
    /// 每次启动随机生成，只写进各个 run 工作目录里的 hook 配置。没有它，
    /// 同机上的任何进程都能替 run 批准工具调用。
    internal_token: String,
    /// 远端动作的落点。`None` 表示这个部署没准备 agent 二进制。
    pub host_exec: Option<ai_task_runtime::HostExecConfig>,
    /// 是否强制登录。关掉时所有请求以 admin 身份通过。
    pub require_auth: bool,
    /// 口令最小长度。只有回环部署才允许低于默认值，由启动校验保证。
    pub min_password_len: usize,
}

/// 从配置里来的那部分。收成一个结构体，让调用点能一眼看出谁是谁
/// ——一串位置参数里把两个 bool 写反了是不会被编译器发现的。
pub struct Settings {
    pub workspace_id: WorkspaceId,
    pub internal_token: String,
    pub host_exec: Option<ai_task_runtime::HostExecConfig>,
    pub require_auth: bool,
    pub min_password_len: usize,
}

impl AppState {
    #[must_use]
    pub fn new(
        store: ai_task_store::Store,
        bus: EventBus,
        engine: RunEngine,
        settings: Settings,
    ) -> Self {
        let Settings {
            workspace_id,
            internal_token,
            host_exec,
            require_auth,
            min_password_len,
        } = settings;
        Self(Arc::new(Inner {
            store,
            bus,
            supervisor: RunSupervisor::new(engine),
            workspace_id,
            internal_token,
            host_exec,
            require_auth,
            min_password_len,
        }))
    }

    /// 记一条审计。**审计写失败不能让业务失败**——因为审计写不进去而拒绝一次
    /// 合法的操作，会让人第一时间想把审计关掉。
    pub async fn audit(
        &self,
        action: &'static str,
        target_kind: &'static str,
        target_id: impl Into<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) {
        let entry = ai_task_store::AuditEntry {
            workspace_id: self.workspace_id,
            // M5 还没接认证。接上之后这里换成会话里的 user id。
            actor_id: None,
            action,
            target_kind,
            target_id: target_id.into(),
            before,
            after,
            request_id: Some(crate::middleware::request_id::current()),
        };
        if let Err(err) = self.store.audit(entry).await {
            tracing::warn!(action, error = %err, "审计写入失败");
        }
    }

    /// 常数时间比较内部令牌。
    ///
    /// 不用 `==`：短路比较会通过耗时泄漏前缀，让令牌可以被逐字节猜出来。
    #[must_use]
    pub fn verify_internal_token(&self, presented: &str) -> bool {
        let expected = self.internal_token.as_bytes();
        let actual = presented.as_bytes();
        // 长度不同直接判否，但仍然走完整个循环，避免长度也变成侧信道
        let mut diff = u8::from(expected.len() != actual.len());
        for (index, byte) in expected.iter().enumerate() {
            diff |= byte ^ actual.get(index).copied().unwrap_or(0);
        }
        diff == 0
    }
}

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
