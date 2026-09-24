//! REST 接口的请求/响应类型。
//!
//! 契约细节见 `docs/adr/0002-api-contract.md`。几条贯穿全局的约定：
//!
//! - **错误**：统一信封 [`ApiError`]，`code` 是契约，`message` 不是（可以随时改文案）
//! - **状态码**：400 = 报文解析不了；**422 = 报文合法但语义非法**（全项目只用 422，
//!   不和 400 混用）；403 与 404 的取舍是「暴露存在性本身算泄漏时返回 404」
//! - **分页**：一律游标，排序键是 `(created_at, id)` 这种唯一且稳定的组合；
//!   `limit` 服务端封顶；不默认返回总数（大表上是全扫）
//! - **幂等**：创建/触发类 POST 走 `Idempotency-Key` 头，不放 body
//! - **并发**：更新走 `ETag` / `If-Match` 头，不放 body

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{ApprovalId, HostId, RunId, ScheduleId, TaskId, TaskVersionId};
use crate::money::UsdMicros;
use crate::spec::{DagSpec, MisfirePolicy, OverlapPolicy, RunStatus, TriggerKind};

/// 统一错误信封。
///
/// 刻意不用 RFC 9457 的字段名（`type`/`title`/`detail`/`instance`）：本仓库其它
/// 服务（见 `transfer-app/src/error.rs`）已经用 `{code, message}`，没有客户端
/// 消费那几个额外成员，跨服务一致比贴合规范更有价值。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiError {
    /// 稳定的机器可读原因。**这是契约**：新增 code 是兼容变更，
    /// 改变已有 code 的触发条件是破坏性变更。
    pub code: String,
    /// 人读文案。不是契约，随时可能改，客户端不要拿它做分支。
    pub message: String,
    /// 与服务端日志关联用，对应 `x-request-id` 响应头。
    pub request_id: String,
    /// 字段级校验失败。**一次返回全部**，不是只报第一个。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<FieldError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FieldError {
    /// 出错字段的路径，如 `nodes[2].config.prompt`。
    pub field: String,
    pub code: String,
    pub message: String,
}

/// 游标分页的一页结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// 下一页的游标。`None` 表示到底了。
    ///
    /// 对客户端不透明：里面编码的是 `(created_at, id)`，格式随时可能变。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 列表接口的分页参数。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    /// 上一页返回的 `next_cursor`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 每页条数。服务端封顶 [`PageQuery::MAX_LIMIT`]，超出直接按上限截断。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl PageQuery {
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MAX_LIMIT: u32 = 200;

    /// 生效的页大小。
    #[must_use]
    pub fn effective_limit(&self) -> u32 {
        self.limit
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(1, Self::MAX_LIMIT)
    }
}

/// `GET /api/v1/healthz`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Health {
    pub status: HealthStatus,
    pub version: String,
    /// 各依赖的探测结果。`healthz` 只看进程存活，`readyz` 会填这个。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Degraded,
    Down,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HealthCheck {
    pub name: String,
    pub status: HealthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub latency_ms: u64,
}

// ---------------------------------------------------------------- 任务

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskSummary {
    pub id: TaskId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 当前生效的版本。run 绑定的是版本快照，不是任务本身。
    pub current_version_id: TaskVersionId,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 乐观锁版本号，同时作为 `ETag` 的取值。
    pub version: i64,
    /// 最近一次 run 的状态，列表页直接展示，省一次查询。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<RunSummary>,
    /// 最近几次 run（最多 10 次），新的在前。列表页画成色块，一眼看出这个任务稳不稳。
    ///
    /// 和 `last_run` 一样**只有列表接口填**；详情、创建、更新的响应里没有。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_runs: Vec<RecentRun>,
}

/// 一次 run 最少的那点信息：够画一个色块，点进去能跳到它。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecentRun {
    pub id: RunId,
    pub status: RunStatus,
    /// 影子执行单独标出来：它是用来试提示词的，失败了不代表任务本身不稳。
    pub dry_run: bool,
    pub created_at: DateTime<Utc>,
}

/// `GET /api/v1/tasks/{id}` —— 带上当前版本的完整编排定义。
///
/// 列表接口不返回 `spec`：一份 DAG 可能有几十个节点，列表页用不上却要传，
/// 白白拖慢首屏。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskDetail {
    #[serde(flatten)]
    pub summary: TaskSummary,
    pub spec: DagSpec,
    /// 这个任务额外应用的规则名。全局规则不在这里。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// 定义快照的版本号，从 1 开始。
    pub version_no: i32,
}

/// `POST /api/v1/tasks` —— 需要 `Idempotency-Key` 头。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct CreateTask {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub spec: DagSpec,
    /// 要应用的规则 ID/名字。全局规则始终生效，这里只列任务级追加的。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// `PUT /api/v1/tasks/{id}` —— 需要 `If-Match` 头，命中即产生新版本。
///
/// 语义是整体替换（PUT），不是字段合并（PATCH）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct UpdateTask {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub spec: DagSpec,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------- 定时

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Schedule {
    pub id: ScheduleId,
    pub task_id: TaskId,
    pub cron: String,
    /// IANA 时区名，如 `Asia/Shanghai`。DST 的跳过/重复时刻由它决定。
    pub timezone: String,
    pub misfire: MisfirePolicy,
    pub overlap: OverlapPolicy,
    /// 触发抖动上限（秒），避免整点惊群。
    pub jitter_s: u32,
    pub enabled: bool,
    /// 下一次触发时刻。调度器就是按这个字段领取的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire_at: Option<DateTime<Utc>>,
}

/// `GET /api/v1/schedules/preview` 的查询参数。
///
/// 编辑定时时边敲边看：表达式写没写对，看接下来几次在什么时候最直接。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct SchedulePreviewQuery {
    /// 5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周）。
    pub cron: String,
    /// IANA 时区名，如 `Asia/Shanghai`。
    pub timezone: String,
    /// 要看接下来几次，1–10，默认 5。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

/// 接下来几次触发。表达式或时区不合法时接口返回 422，字段是 `cron` / `timezone`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SchedulePreview {
    pub fires: Vec<ScheduleFire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ScheduleFire {
    pub at: DateTime<Utc>,
    /// 按该时区的本地时间，形如 `2026-09-25 02:00:00 CST`。
    pub local: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct UpsertSchedule {
    pub cron: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub misfire: MisfirePolicy,
    #[serde(default)]
    pub overlap: OverlapPolicy,
    #[serde(default)]
    pub jitter_s: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_timezone() -> String {
    "UTC".into()
}

// ---------------------------------------------------------------- Run

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunSummary {
    pub id: RunId,
    pub task_id: TaskId,
    pub task_version_id: TaskVersionId,
    pub status: RunStatus,
    pub trigger: TriggerKind,
    pub dry_run: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub cost_usd: UsdMicros,
    /// 失败原因。只在终态且非成功时有值。
    ///
    /// 加这个字段是为了让轮询 summary 的客户端不必为了知道"为什么失败"
    /// 去拉整条事件流。新增可选字段是兼容变更。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 事件日志的最大 seq。SSE 客户端用它判断自己落后多少。
    pub max_seq: i64,
}

/// `GET /api/v1/runs` 的一行：摘要加任务名。
///
/// 列表每一行都要显示任务名。以前客户端为此再拉一遍任务表逐行去找，
/// 任务表又只取了第一页——第 51 个任务的执行记录显示成一串 id。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunListItem {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub task_name: String,
}

/// `GET /api/v1/runs/{id}` —— 一次执行的全部元信息。
///
/// 比列表多出来的都是详情页才用得上的：输入（重跑要原样带上）、输出（结论）、
/// 版本号（编排图要画**这次执行用的那一版**，不是任务现在的样子）、谁触发的。
/// 是 `RunSummary` 的超集，只多不少。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunDetail {
    #[serde(flatten)]
    pub summary: RunSummary,
    pub task_name: String,
    /// 这次执行绑定的版本号。
    pub version_no: i32,
    /// 任务**现在**的版本号。和 `version_no` 不同，说明任务在这次执行之后被改过。
    pub current_version_no: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
    /// run 的最终输出：拓扑序里最后一个成功节点的输出。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_to: Option<RunId>,
    /// 驱动这次执行的 claude CLI 版本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    /// 手动触发的人（显示名）。定时触发、没开认证时没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
    /// 定时触发时是哪条定时。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<ScheduleId>,
}

/// `GET /api/v1/tasks/{id}/versions/{no}` —— 一个版本的编排快照。版本不可变。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskVersion {
    pub task_id: TaskId,
    pub version_no: i32,
    pub spec: DagSpec,
    /// 这个版本额外挂的规则名。全局规则不在这里。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// `POST /api/v1/tasks/{id}/runs` —— 需要 `Idempotency-Key` 头，返回 202。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct TriggerRun {
    /// 传给 DAG 的自由参数，按 [`DagSpec::input_schema`] 校验。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<serde_json::Value>,
    /// 影子执行：所有副作用工具只记录意图不执行。
    #[serde(default)]
    pub dry_run: bool,
    /// 与哪个基线 run 做结构化 diff。配合 `dry_run` 用于换 prompt / 换模型前的验证。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_to: Option<RunId>,
}

/// `GET /api/v1/overview` 的查询参数。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct OverviewQuery {
    /// 统计窗口，小时。默认 24，上限 720（30 天）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_hours: Option<u32>,
}

impl OverviewQuery {
    pub const DEFAULT_WINDOW_HOURS: u32 = 24;
    pub const MAX_WINDOW_HOURS: u32 = 720;

    #[must_use]
    pub fn effective_window_hours(&self) -> u32 {
        self.window_hours
            .unwrap_or(Self::DEFAULT_WINDOW_HOURS)
            .clamp(1, Self::MAX_WINDOW_HOURS)
    }
}

/// 首页那一屏要的全部数字，一次算完。
///
/// **这个接口首先是个正确性修复，不只是少发几个请求。**
/// 之前首页是拉最近 200 条 run 回浏览器里算 24 小时的次数、失败数和花费——
/// 实例一忙，第 201 条之后的就静静地不算了，而界面上那三个数字看不出自己是错的。
/// 窗口聚合只能在数据库里做。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Overview {
    pub stats: OverviewStats,
    /// 正在跑和排队中的，最近的在前。
    pub live: Vec<OverviewRun>,
    /// 最近结束的。
    pub recent: Vec<OverviewRun>,
    /// 接下来会自己触发的定时。
    pub upcoming: Vec<UpcomingFire>,
}

/// 首页顶部那一排指标。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OverviewStats {
    /// 下面几个窗口内的统计按这个小时数算。回显出来，免得客户端自己猜。
    pub window_hours: u32,
    /// 窗口内的 run 总数。
    pub runs: i64,
    /// 窗口内落在失败类终态（failed / timed_out / budget_exceeded / resource_exceeded）的。
    pub failed: i64,
    /// 窗口内的模型花费合计。
    pub spend_usd: UsdMicros,
    /// 当前在跑的（不受窗口限制）。
    pub running: i64,
    /// 当前排队的（不受窗口限制）。
    pub queued: i64,
    /// 当前待决的审批（不受窗口限制）。
    pub pending_approvals: i64,
    /// 任务总数。
    pub tasks: i64,
}

/// 首页列表里的一行。
///
/// 比 [`RunSummary`] 少几个字段、多一个 `task_name`：首页每一行都要显示任务名，
/// 而客户端为此再拉一遍任务表、逐行 `find` 是没必要的。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OverviewRun {
    pub id: RunId,
    pub task_id: TaskId,
    pub task_name: String,
    pub status: RunStatus,
    pub trigger: TriggerKind,
    pub dry_run: bool,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub cost_usd: UsdMicros,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 接下来会自己响的一个定时。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UpcomingFire {
    pub schedule_id: ScheduleId,
    pub task_id: TaskId,
    pub task_name: String,
    pub cron: String,
    pub next_fire_at: DateTime<Utc>,
}

/// `GET /api/v1/runs` 的查询参数。服务端直接按这个类型解析，所以它就是契约本身。
///
/// 多选用**逗号分隔**（`status=failed,timed_out`），不用重复的键：查询串的解析
/// 不认重复键。认不出的值整个请求 422——拼错一个状态名不能悄悄变成「不过滤」。
/// 未声明的参数会被拒绝而不是忽略，理由同上。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct RunFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// 逗号分隔的 [`RunStatus`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// 逗号分隔的 [`TriggerKind`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// 只要这个时刻（含）之后创建的，RFC 3339。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// 上一页的 `next_cursor`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 每页条数，1–200，默认 50。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `POST /api/v1/runs/{id}/nodes/{key}/approve`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecision {
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------- 内部：策略判决

/// `POST /internal/policy/decide` 的请求。
///
/// 调用方是 `ai-task-mcp` 的远端工具代理和 Claude Code 的 `PreToolUse` hook，
/// **不对外暴露**。工具调用在这个接口返回前是阻塞的。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct PolicyRequest {
    pub run_id: RunId,
    pub node_key: String,
    pub tool_use_id: String,
    pub tool: String,
    pub input: serde_json::Value,
    /// 动作的目标主机。`None` 表示中心节点本机。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<HostId>,
}

/// `POST /internal/remote/call` 的请求。
///
/// 每个远端动作都必须经过这个接口——**这是策略无法被绕过的原因**：
/// MCP 代理进程里没有 SSH 凭据，它除了打这个接口没有别的路。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemoteCallRequest {
    pub run_id: RunId,
    pub node_key: String,
    /// 模型这次调用的 id。判决和结果都挂在它上面，审计时能对上。
    pub tool_use_id: String,
    /// 目标主机。**由中心从任务定义里取，不接受调用方指定**——
    /// 否则模型可以自己换一台没有 prod tag 的机器来绕开策略。
    pub action: RemoteAction,
}

/// 允许在远端做的事。刻意只有这四样。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteAction {
    /// 在目标机上跑一条命令。
    RemoteBash {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_s: Option<u32>,
    },
    RemoteRead {
        path: String,
    },
    RemoteWrite {
        path: String,
        content: String,
    },
    RemoteGlob {
        pattern: String,
    },
}

impl RemoteAction {
    /// 工具名。策略匹配器按它匹配，必须和 serde tag 逐字一致。
    #[must_use]
    pub fn tool(&self) -> &'static str {
        match self {
            Self::RemoteBash { .. } => "remote_bash",
            Self::RemoteRead { .. } => "remote_read",
            Self::RemoteWrite { .. } => "remote_write",
            Self::RemoteGlob { .. } => "remote_glob",
        }
    }
}

/// `POST /internal/remote/call` 的响应。
///
/// **策略拒绝不是 HTTP 错误**：它是一个模型必须看见并据此调整的正常结果。
/// 返回 4xx 会让 MCP 客户端把它当成传输故障，模型只会原样重试。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RemoteCallResponse {
    /// 动作是否真的执行了。被策略拒绝、或影子执行下，这里是 false。
    pub executed: bool,
    /// 给模型看的正文：执行结果，或者被拒的原因。
    pub content: String,
    /// 命令退出码。非 `remote_bash` 或未执行时为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
}

/// `POST /internal/policy/decide` 的响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PolicyResponse {
    /// `ask` 只会作为中间态出现在事件日志里；这个接口在人工决策完成后
    /// 才返回，所以这里只会是 `allow` 或 `deny`。
    pub allowed: bool,
    /// 拒绝原因。会作为 `tool_result` 回给模型，让它自己调整，
    /// 而不是把调用挂死或直接失败。
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// 影子执行下为 true：调用方应当记录意图但不真的执行。
    pub dry_run: bool,
    /// 命中 `ask`：已建好审批，调用方应当轮询 `/internal/policy/await`
    /// 直到有结论或者自己的截止时间到。
    ///
    /// **不用一个长请求把结论等回来**：那样 hook 的超时必须放大到审批窗口那么长，
    /// 于是服务端一旦卡住，每次工具调用都要挂十分钟才失败，而不是二十秒内
    /// 快速失败关门。轮询让每个请求都是短的，超时语义不用动。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval_id: Option<ApprovalId>,
}

/// `POST /internal/policy/await` 的请求：等一条已建好的审批。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AwaitApprovalRequest {
    pub run_id: RunId,
    pub approval_id: ApprovalId,
}

/// `POST /internal/policy/await` 的响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AwaitApprovalResponse {
    /// 还没有结论，调用方应当再问一次。
    pub pending: bool,
    /// `pending` 为 false 时才有意义。
    pub approved: bool,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_limit_is_capped_and_floored() {
        assert_eq!(PageQuery::default().effective_limit(), 50);
        assert_eq!(
            PageQuery {
                limit: Some(10_000),
                ..Default::default()
            }
            .effective_limit(),
            PageQuery::MAX_LIMIT
        );
        assert_eq!(
            PageQuery {
                limit: Some(0),
                ..Default::default()
            }
            .effective_limit(),
            1
        );
    }

    #[test]
    fn error_envelope_omits_empty_details() {
        let json = serde_json::to_value(ApiError {
            code: "not_found".into(),
            message: "任务不存在".into(),
            request_id: "req-1".into(),
            details: vec![],
        })
        .expect("序列化");
        assert!(json.get("details").is_none());
        assert_eq!(json["code"], "not_found");
    }

    #[test]
    fn unknown_query_params_are_rejected() {
        // 拼错 task_id 不能静默变成「不过滤」
        let json = serde_json::json!({ "taskid": "x" });
        assert!(serde_json::from_value::<RunFilter>(json).is_err());
    }

    #[test]
    fn trigger_defaults_to_a_real_run_not_a_shadow_one() {
        let t: TriggerRun = serde_json::from_value(serde_json::json!({})).expect("反序列化");
        assert!(!t.dry_run);
        assert!(t.compare_to.is_none());
    }

    #[test]
    fn run_cost_serializes_as_a_decimal_string() {
        let json = serde_json::to_value(RunSummary {
            id: RunId::new(),
            task_id: TaskId::new(),
            task_version_id: TaskVersionId::new(),
            status: RunStatus::Running,
            trigger: TriggerKind::Schedule,
            dry_run: false,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            cost_usd: UsdMicros(1_234_500),
            error: None,
            max_seq: 42,
        })
        .expect("序列化");
        assert_eq!(json["cost_usd"], serde_json::json!("1.234500"));
        assert!(json.get("started_at").is_none());
        assert!(
            json.get("error").is_none(),
            "没有错误时不该出现一个 null 字段"
        );
    }
}
