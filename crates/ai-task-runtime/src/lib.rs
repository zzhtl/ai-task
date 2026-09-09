//! 调度与执行编排。
//!
//! 依赖方向：runtime → core / store / exec。**反向依赖是错误。**
//! 状态推导只有一处实现，在 `ai_task_core::RunState`；这里不得另起一套。

pub mod approval;
pub mod cron;
pub mod dag;
pub mod engine;
pub mod host_exec;
pub mod scheduler;
pub mod sink;

pub use approval::Verdict;
pub use cron::{CronError, CronSchedule, DueSet};
pub use dag::{DagOutcome, NodeResult, NodeRunContext, NodeRunner, run_dag};
pub use engine::{EngineError, HookSettings, RunEngine, reap_orphaned_runs};
pub use host_exec::{Command, HostExecConfig, HostExecError, HostExecOutcome, run_command};
pub use scheduler::{Scheduler, SchedulerError, Tick};
pub use sink::EventSink;
