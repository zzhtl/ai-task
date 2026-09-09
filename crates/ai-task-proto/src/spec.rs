//! 任务编排规格（DAG spec）。
//!
//! 一份 `DagSpec` 会被冻结进 `task_versions`，run 引用的是版本快照而不是任务本身。
//! 否则改了任务定义后，历史 run 无法解释、无法回放、漂移检测也失去基线。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{HostId, NodeKey};
use crate::money::UsdMicros;

/// 一个任务版本的完整编排定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct DagSpec {
    pub nodes: Vec<NodeSpec>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    /// 触发时可传入的自由参数的 JSON Schema。`None` 表示不接受输入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// 整个 run 的成本上限。超出即熔断，run 落 `budget_exceeded`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<UsdMicros>,
    /// 整个 run 的墙钟上限（秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
}

/// DAG 里的一个节点。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct NodeSpec {
    pub key: NodeKey,
    /// 展示名。为空时界面回退到 `key`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 节点类型与配置。
    ///
    /// 刻意**不用** `#[serde(flatten)]` 把 `kind` 提到顶层：serde 的 `flatten`
    /// 与 `deny_unknown_fields` 互斥，一旦 flatten 就必须放弃整个 spec 的字段
    /// 拼写检查。这是人手写的 YAML，「配了但没生效」是最坏的失败模式，
    /// 换一层嵌套换全量拼写检查是划算的。
    pub config: NodeConfig,
    /// 节点输入：名字 → 取值来源。名字会以变量形式出现在 prompt / command 里。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputRef>,
    /// 输出契约。校验不过时按 [`RetryPolicy::feed_error_to_model`] 决定是否回喂重试。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub retry: RetryPolicy,
    #[serde(default)]
    pub on_failure: OnFailure,
    /// 单个节点的墙钟上限（秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
    /// 在哪台机器上执行。`None` 等同 [`HostSelector::Local`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostSelector>,
    /// 资源上限。
    ///
    /// 只在目标机能强制时才真的生效（需要 cgroup v2 + systemd）；
    /// 降级的机器上会记账但不设限，事件日志里会明确写出来——
    /// 谎称限额在生效比不限更糟。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ResourceLimits>,
}

/// 节点执行的资源上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    /// 内存上限（MiB）。超出由内核直接 kill，run 落 `resource_exceeded`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u32>,
    /// CPU 配额，百分比。200 表示最多用满两个核。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_percent: Option<u32>,
    /// 进程数上限。挡住 fork 炸弹。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids_max: Option<u32>,
}

/// 节点类型与它各自的配置。
///
/// 内部 tag（`kind`）而不是嵌套对象：JSON 更扁平，TS 侧是天然的可辨识联合。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeConfig {
    /// AI 执行节点。产品的主力。
    Ai(AiNode),
    /// 确定性命令。
    Shell(ShellNode),
    /// 校验上游输出。不通过就走 `on_failure` 边。
    Assert(AssertNode),
    /// 人工审批门。
    Approval(ApprovalNode),
    /// 对上游数组扇出，运行时按元素实例化 `template`。
    Map(MapNode),
}

impl NodeConfig {
    /// 节点类型的稳定标识，用于日志、指标和界面图标。
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Ai(_) => "ai",
            Self::Shell(_) => "shell",
            Self::Assert(_) => "assert",
            Self::Approval(_) => "approval",
            Self::Map(_) => "map",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct AiNode {
    pub prompt: String,
    #[serde(default)]
    pub executor: ExecutorKind,
    /// 模型 ID。`None` 时由 executor 取默认值（CLI 用 opus，API 用 haiku）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// 要注入上下文的 skill 名字。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// 允许使用的内置工具。
    ///
    /// 不给就用执行器的默认白名单（只读）。**这只是第一道闸门**：
    /// 真正的约束是工具调用边界上的硬策略，写类工具即使在这里被放进来，
    /// 也还要过策略层那一关。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// agent 循环的最大轮数。防止模型在工具调用里打转。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// 本节点的成本上限，独立于 run 级预算。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<UsdMicros>,
}

/// 确定性命令。
///
/// 命令由**操作者**写在编排里，不是模型生成的，所以不过工具调用策略层
/// （那一层管的是模型的自由裁量）。约束它的是节点上的 `limits` 和 `host`：
/// 跑在哪台机器上、能用多少资源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ShellNode {
    /// 交给 `sh -c` 的命令行。注意 Debian/Ubuntu 上那是 dash 不是 bash。
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

/// 校验方式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssertNode {
    /// 用 JSON Schema 校验上游输出。最便宜，优先用这个。
    JsonSchema { schema: serde_json::Value },
    /// JSONPath 取值后做比较。
    Expression {
        left: InputRef,
        cmp: Cmp,
        right: serde_json::Value,
    },
    /// LLM-as-judge。只有前两种表达不了时才用——它本身也是非确定性的。
    Judge {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ApprovalNode {
    /// 审批卡片的标题。
    pub title: String,
    /// 等待人工决策的上限（秒）。
    #[serde(default = "default_approval_timeout_s")]
    pub timeout_s: u32,
    #[serde(default)]
    pub on_timeout: ApprovalTimeout,
}

fn default_approval_timeout_s() -> u32 {
    900
}

/// 超时未决时的默认动作。默认拒绝——审批门的意义就在于「没人点头就不做」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalTimeout {
    #[default]
    Deny,
    Approve,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MapNode {
    /// 要遍历的数组。取值不是数组时节点直接失败。
    pub over: InputRef,
    /// 每个元素实例化一份。模板里用 [`InputRef::MapItem`] 引用当前元素。
    pub template: Box<NodeSpec>,
    #[serde(default = "default_map_parallel")]
    pub max_parallel: u32,
    /// 元素数上限。**必填且有默认值**：`over` 来自 AI 输出，
    /// 一个失控的数组能瞬间放大成几千个 run 节点。
    #[serde(default = "default_map_max_items")]
    pub max_items: u32,
}

fn default_map_parallel() -> u32 {
    4
}

fn default_map_max_items() -> u32 {
    100
}

/// 节点取值的来源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "from", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputRef {
    /// 上游节点输出里的一段。`path` 是 JSONPath（RFC 9535）。
    Node { node: NodeKey, path: String },
    /// 触发 run 时传入的参数。
    RunInput { path: String },
    /// map 展开后的当前元素。只在 [`MapNode::template`] 内部合法。
    MapItem,
    /// 写死的常量。
    Literal { value: serde_json::Value },
}

/// 节点之间的依赖边。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: NodeKey,
    pub to: NodeKey,
    #[serde(default)]
    pub when: EdgeCondition,
}

/// 边的触发条件。默认只在上游成功时放行。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EdgeCondition {
    #[default]
    OnSuccess,
    /// 失败分支。取代了「on_failure: branch」这种额外语义——失败路径就是一条边。
    OnFailure,
    /// 无论上游成败都放行。
    Always,
    /// 对上游输出求值。
    Expr {
        left: InputRef,
        cmp: Cmp,
        right: serde_json::Value,
    },
}

/// 比较运算符。刻意不做通用表达式语言：规则要可审计、可静态分析。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    Contains,
    Exists,
    NotExists,
}

/// 节点重试策略。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    /// 含首次在内的总尝试次数。1 表示不重试。
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: u32,
    #[serde(default = "default_backoff_factor")]
    pub backoff_factor: f32,
    /// 把上次的失败原因（含 `output_schema` 的校验错误）作为下一轮输入回喂给模型。
    ///
    /// 这是「节点契约化」的执行点：让 AI 看见自己错在哪，而不是原样重来。
    #[serde(default = "default_true")]
    pub feed_error_to_model: bool,
}

fn default_max_attempts() -> u32 {
    1
}
fn default_backoff_ms() -> u32 {
    1_000
}
fn default_backoff_factor() -> f32 {
    2.0
}
fn default_true() -> bool {
    true
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff_ms: default_backoff_ms(),
            backoff_factor: default_backoff_factor(),
            feed_error_to_model: true,
        }
    }
}

/// 节点最终失败后，整个 run 怎么走。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// 取消其余在跑的节点，run 落 `failed`。
    #[default]
    FailFast,
    /// 只标记本节点失败，其余分支继续。
    Continue,
}

/// 节点在哪台机器上执行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "on", rename_all = "snake_case", deny_unknown_fields)]
pub enum HostSelector {
    /// 中心节点本机。
    Local,
    /// 指定主机。
    Host { host_id: HostId },
    /// 命中 tag 的所有主机各跑一次。
    Tag { tag: String },
}

/// AI 执行内核。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// 驱动 Claude Code CLI。开放式任务用这个。
    #[default]
    ClaudeCode,
    /// 自建 Messages API 循环。分类/抽取/判断这类封闭式推理用这个，更便宜更快。
    Api,
}

/// 思考深度。对应 Messages API 的 `output_config.effort`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// 定时器错过触发点后的补偿策略（服务停过一段时间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MisfirePolicy {
    /// 全部丢弃，只等下一个正常触发点。
    Skip,
    /// 只补一次，然后对齐到下一个正常触发点。
    #[default]
    FireOnce,
    /// 错过几次补几次。会造成惊群，只在补数据类任务上用。
    FireAll,
}

/// 上一次还没跑完时，新触发点怎么处理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum OverlapPolicy {
    /// 并发跑。
    Allow,
    /// 丢弃本次触发。
    #[default]
    Skip,
    /// 排队，等上一次结束再跑。
    Queue,
}

/// run 的触发来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Manual,
    Schedule,
    Api,
    /// 由上游 run 触发（`map` 展开或子任务）。
    Parent,
}

/// run 的终态/进行态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    /// 命中成本上限被熔断。
    BudgetExceeded,
    /// 目标机 cgroup 限额被击穿，执行被 kill。
    ResourceExceeded,
}

impl RunStatus {
    /// 终态不再变化，事件流可以关闭。
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }

    /// 是否算作成功。只有 `Succeeded`。
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// 单个节点实例的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// 上游未就绪。节点刚被创建时就是这个状态。
    #[default]
    Pending,
    /// 依赖已满足，等 worker 领取。
    Ready,
    Running,
    /// 命中 `ask` 策略或走到 approval 节点，等人。
    AwaitingApproval,
    Succeeded,
    Failed,
    /// 入边条件不满足，整支被跳过。
    Skipped,
    Cancelled,
}

impl NodeStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }
}

/// 策略判决结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEffect {
    Allow,
    Deny,
    /// 转人工审批，工具调用阻塞等待。
    Ask,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ai(key: &str, prompt: &str) -> NodeSpec {
        NodeSpec {
            key: NodeKey::parse(key).expect("合法 key"),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: prompt.into(),
                executor: ExecutorKind::default(),
                model: None,
                effort: None,
                skills: vec![],
                tools: vec![],
                max_turns: None,
                budget_usd: None,
            }),
            inputs: BTreeMap::new(),
            output_schema: None,
            retry: RetryPolicy::default(),
            on_failure: OnFailure::default(),
            timeout_s: None,
            host: None,
            limits: None,
        }
    }

    #[test]
    fn node_config_is_a_discriminated_union_under_config() {
        let spec = ai("probe", "巡检");
        let json = serde_json::to_value(&spec).expect("序列化");
        assert_eq!(json["config"]["kind"], "ai");
        assert_eq!(json["config"]["prompt"], "巡检");
    }

    #[test]
    fn optional_fields_are_omitted_not_nulled() {
        let json = serde_json::to_value(ai("a", "x")).expect("序列化");
        for absent in [
            "name",
            "model",
            "effort",
            "output_schema",
            "timeout_s",
            "host",
        ] {
            assert!(
                json.get(absent).is_none(),
                "{absent} 应当被省略而不是序列化成 null"
            );
        }
    }

    #[test]
    fn minimal_node_json_round_trips_with_all_defaults() {
        let json = serde_json::json!({
            "key": "a",
            "config": { "kind": "ai", "prompt": "hi" }
        });
        let node: NodeSpec = serde_json::from_value(json).expect("反序列化");
        assert_eq!(node.retry.max_attempts, 1);
        assert!(node.retry.feed_error_to_model);
        assert_eq!(node.on_failure, OnFailure::FailFast);
        assert_eq!(node.config.kind(), "ai");
    }

    #[test]
    fn unknown_fields_are_rejected_not_silently_dropped() {
        // 拼错的字段必须报错，否则「配了但没生效」会静默发生
        let typo_in_node = serde_json::json!({
            "key": "a", "config": { "kind": "shell", "command": "ls" }, "retries": 3
        });
        assert!(serde_json::from_value::<NodeSpec>(typo_in_node).is_err());

        let typo_in_config = serde_json::json!({
            "key": "a", "config": { "kind": "shell", "command": "ls", "workingdir": "/tmp" }
        });
        assert!(serde_json::from_value::<NodeSpec>(typo_in_config).is_err());
    }

    #[test]
    fn map_node_carries_an_item_cap_by_default() {
        let json = serde_json::json!({
            "key": "fan",
            "config": {
                "kind": "map",
                "over": { "from": "node", "node": "probe", "path": "$.anomalies" },
                "template": { "key": "fix", "config": { "kind": "ai", "prompt": "修" } }
            }
        });
        let node: NodeSpec = serde_json::from_value(json).expect("反序列化");
        let NodeConfig::Map(m) = node.config else {
            panic!("应当是 map 节点")
        };
        assert_eq!(
            m.max_items, 100,
            "AI 产出的数组必须有上限，否则会放大成几千个节点"
        );
        assert_eq!(m.max_parallel, 4);
    }

    #[test]
    fn edge_defaults_to_on_success() {
        let e: Edge = serde_json::from_value(serde_json::json!({ "from": "a", "to": "b" }))
            .expect("反序列化");
        assert_eq!(e.when, EdgeCondition::OnSuccess);
    }

    #[test]
    fn run_status_terminality() {
        assert!(!RunStatus::Queued.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        for s in [
            RunStatus::Succeeded,
            RunStatus::Failed,
            RunStatus::Cancelled,
            RunStatus::TimedOut,
            RunStatus::BudgetExceeded,
            RunStatus::ResourceExceeded,
        ] {
            assert!(s.is_terminal(), "{s:?} 应当是终态");
        }
        assert!(RunStatus::Succeeded.is_success());
        assert!(!RunStatus::TimedOut.is_success());
    }

    #[test]
    fn enums_serialize_as_snake_case_strings() {
        assert_eq!(
            serde_json::to_value(RunStatus::BudgetExceeded).expect("序列化"),
            serde_json::json!("budget_exceeded")
        );
        assert_eq!(
            serde_json::to_value(ExecutorKind::ClaudeCode).expect("序列化"),
            serde_json::json!("claude_code")
        );
    }
}
