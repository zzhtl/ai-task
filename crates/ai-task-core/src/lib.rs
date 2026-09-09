//! ai-task 的领域逻辑。
//!
//! **这个 crate 不做任何 IO**：没有数据库、没有网络、没有文件系统、没有时钟
//! （时间一律由调用方传入）。所有逻辑都是纯函数，因此全部可以用普通单测覆盖，
//! 不需要起容器、不需要 mock。
//!
//! 三块内容：
//! - [`dag`]：任务编排的结构校验与拓扑序
//! - [`run_state`]：把事件日志 fold 成 run 的当前状态

pub mod dag;
pub mod drift;
pub mod eval;
pub mod policy;
pub mod run_state;

pub use dag::{DagError, ValidatedDag};
pub use eval::{EvalContext, EvalError, NodeOutputs, resolve, validate_output};
pub use policy::{
    Decision, DefaultPolicy, Pattern, PolicyRule, PolicySet, RuleScope, ToolCall, ToolMatcher,
};
pub use run_state::{NodeState, RunState};
