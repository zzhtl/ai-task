//! PostgreSQL 持久化层。
//!
//! **整个仓库里只有这个 crate 写 SQL。** 上层拿到的是领域类型，不是行。
//!
//! 关于连接池与代理的一条硬约束：**不要把 PgBouncer 以 transaction 模式挡在
//! 前面**。事件的跨副本扇出依赖 `LISTEN`/`NOTIFY`，transaction pooling 会直接
//! 破坏 LISTEN 的会话语义。需要池代理时用 session 模式。

pub mod approvals;
pub mod audit;
pub mod auth;
pub mod crypto;
pub mod events;
pub mod hosts;
pub mod pool;
pub mod rules;
pub mod runs;
pub mod schedules;
pub mod tasks;

pub use approvals::{Approval, DecisionOutcome, NewApproval};
pub use audit::{AuditEntry, AuditRecord};
pub use auth::{NewUser, Principal, Role};
pub use events::{EVENTS_CHANNEL, PendingEvent};
pub use hosts::{Host, MetricSample, NewHost};
pub use pool::{Store, StoreConfig, StoreError};
pub use rules::{NewRule, NewSkill, PromptRule, RuleKind, RuleRow, Skill};
pub use runs::{NewRun, RunOutcome, RunRecord};
pub use schedules::{DueSchedule, NewSchedule, SchedulePosition};
pub use tasks::{NewTask, TaskRecord, TaskVersionRecord};

/// 编译期嵌入的迁移集。
///
/// 用 `sqlx::migrate!()` 而不是运行时读目录：迁移文件缺失会变成编译错误，
/// 而不是等到生产环境启动时才发现（这也是 `biga` 用 `include_str!` 手写迁移
/// 执行器想达到的效果，但不必自己写执行器）。
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
