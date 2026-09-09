//! Run 的执行编排。
//!
//! M1 只做「按拓扑序顺序跑完所有节点」。条件边、重试、`map` 展开、并发
//! 是 M3 的事，但节点循环已经建立在 [`ValidatedDag::topo_order`] 上，
//! 到时候是在这里加分支，不是推倒重来。

use std::collections::BTreeMap;
use std::sync::Arc;

use ai_task_core::{RunState, ValidatedDag};
use ai_task_exec::{
    ExecEvent, ExecOutcome, ExecRequest, Executor, HookConfig, RunWorkdir, SkillFiles, ToolOutcome,
};
use ai_task_proto::{
    LogLevel, NodeConfig, NodeKey, NodeSpec, NodeStatus, PolicyEffect, RunEventBody, RunId,
    RunStatus, UsdMicros, WorkspaceId,
};
use ai_task_store::{PendingEvent, RunOutcome, Store, StoreError};
use tokio::sync::Mutex;

use crate::dag::{NodeResult, NodeRunContext, NodeRunner};
use crate::sink::{EventSink, EventWriter};
use tokio_util::sync::CancellationToken;

/// 取消后等待执行器自行收尾的上限。
///
/// 超时就自己下结论：一个失联的子进程不该让 run 永远挂着，
/// 那会把调度器的并发额度占死。
const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// `sleep_until` 的 Option 版本。`None` 时永不就绪，配合 select 的守卫条件用。
async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Workdir(#[from] ai_task_exec::WorkdirError),

    #[error("任务版本 {version} 的编排定义非法：{detail}")]
    InvalidDag { version: String, detail: String },

    #[error("启动执行器失败：{0}")]
    Exec(#[from] ai_task_exec::ExecError),
}

/// 策略 hook 的接入信息。
#[derive(Debug, Clone)]
pub struct HookSettings {
    /// hook 程序（一般是 ai-task 自己的绝对路径）。
    pub program: std::path::PathBuf,
    /// 判决接口地址。
    pub endpoint: String,
    /// 远端动作接口地址。
    pub remote_endpoint: String,
    /// 内部令牌。
    pub token: String,
}

/// 驱动 run 的引擎。
#[derive(Clone)]
pub struct RunEngine {
    store: Store,
    executor: Arc<dyn Executor>,
    /// run 的工作目录的父目录。每个 run 在下面开一个自己的子目录。
    workspace_root: std::path::PathBuf,
    worker_id: String,
    /// `None` 表示不装 hook。**只应该在测试里出现**——生产上没有 hook
    /// 就等于没有策略层。
    hook: Option<HookSettings>,
    /// `None` 表示 shell 节点不可用。缺 agent 二进制时明确报错，
    /// 而不是悄悄退回到直接 `Command::spawn`——那样就没有资源归因和上限了。
    host_exec: Option<crate::host_exec::HostExecConfig>,
}

impl RunEngine {
    #[must_use]
    pub fn new(
        store: Store,
        executor: Arc<dyn Executor>,
        workspace_root: impl Into<std::path::PathBuf>,
        worker_id: impl Into<String>,
    ) -> Self {
        Self {
            store,
            executor,
            workspace_root: workspace_root.into(),
            worker_id: worker_id.into(),
            hook: None,
            host_exec: None,
        }
    }

    /// 装上策略 hook。
    #[must_use]
    pub fn with_hook(mut self, hook: HookSettings) -> Self {
        self.hook = Some(hook);
        self
    }

    /// 跑完一个 run。
    ///
    /// 这个函数**总是**给 run 落一个终态——包括自己出错的时候。半途而废的 run
    /// 会永远停在 running，把调度器的并发额度占死。
    pub async fn execute(
        &self,
        workspace_id: WorkspaceId,
        run_id: RunId,
        cancel: CancellationToken,
    ) {
        if let Err(err) = self.execute_inner(workspace_id, run_id, &cancel).await {
            tracing::error!(%run_id, error = %err, "run 执行失败");
            let terminal = if cancel.is_cancelled() {
                RunStatus::Cancelled
            } else {
                RunStatus::Failed
            };
            // 兜底落终态。这一步再失败就只能记日志了——数据库都不可用时
            // 没有别的地方可以写。
            if let Err(err) = self
                .store
                .finish_run(
                    run_id,
                    RunOutcome::new(terminal, UsdMicros::ZERO).with_error(err.to_string()),
                    PendingEvent::run(RunEventBody::RunFinished {
                        status: terminal,
                        error: Some(err.to_string()),
                        cost_usd: UsdMicros::ZERO,
                    }),
                )
                .await
            {
                tracing::error!(%run_id, error = %err, "连终态都写不进去，run 会停在 running");
            }
        }
    }

    /// 配置 shell 节点的落点。不配的话 shell 节点会明确失败。
    #[must_use]
    pub fn with_host_exec(mut self, config: crate::host_exec::HostExecConfig) -> Self {
        self.host_exec = Some(config);
        self
    }

    async fn execute_inner(
        &self,
        workspace_id: WorkspaceId,
        run_id: RunId,
        cancel: &CancellationToken,
    ) -> Result<(), EngineError> {
        let run = self.store.get_run(workspace_id, run_id).await?;
        let version = self.store.get_task_version(run.task_version_id).await?;
        let dag = ValidatedDag::validate(version.spec.clone()).map_err(|errors| {
            EngineError::InvalidDag {
                version: version.id.to_string(),
                detail: errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("；"),
            }
        })?;

        let workdir = RunWorkdir::prepare(self.workspace_root.join(run_id.to_string())).await?;

        // 软规则：注入 system prompt，只影响模型倾向。
        // 硬策略走 hook，跑在模型之外，上下文影响不到它。
        let prompt_rules = self
            .store
            .prompt_rules_for(workspace_id, &version.rules)
            .await?;
        let system_append = compose_rules(&prompt_rules);

        // skills 落到 .claude/skills/，渐进式披露由 CLI 自己完成
        let skill_names = collect_skill_names(&dag);
        let skills = self
            .store
            .skills_by_name(workspace_id, &skill_names)
            .await?;
        let materialized = workdir
            .write_skills(
                &skills
                    .iter()
                    .map(|s| SkillFiles {
                        name: s.name.clone(),
                        description: s.description.clone(),
                        body: s.body.clone(),
                        files: s.files.clone(),
                    })
                    .collect::<Vec<_>>(),
            )
            .await?;

        let settings_path = match &self.hook {
            Some(hook) => Some(
                workdir
                    .write_hook_settings(&HookConfig {
                        program: hook.program.clone(),
                        endpoint: hook.endpoint.clone(),
                        run_id: run_id.to_string(),
                        token: hook.token.clone(),
                    })
                    .await?,
            ),
            None => {
                tracing::warn!(%run_id, "没有配置策略 hook，本次执行不受策略约束");
                None
            }
        };

        // 声明了远端主机的 AI 节点，各自一份代理配置。
        //
        // 一个节点一次 CLI 调用，也就一个 MCP server 进程——所以目标节点名
        // 可以钉在启动参数里，中心据此从任务定义取该节点声明的主机。
        // 代理拿不到、也改不了这个 node_key。
        let mut mcp_configs: BTreeMap<NodeKey, std::path::PathBuf> = BTreeMap::new();
        for node in dag.spec().nodes.iter().filter(|n| needs_remote_tools(n)) {
            let Some(hook) = &self.hook else {
                // 没有 hook 就没有策略层。宁可让节点失败在"没有远端工具"上，
                // 也不能给模型一套不受约束的远端工具。
                tracing::warn!(%run_id, node = %node.key, "没有配置策略 hook，远端 AI 工具一并禁用");
                break;
            };
            let path = workdir
                .write_mcp_config(
                    node.key.as_str(),
                    &ai_task_exec::McpProxyConfig {
                        program: hook.program.clone(),
                        endpoint: hook.remote_endpoint.clone(),
                        run_id: run_id.to_string(),
                        node_key: node.key.to_string(),
                        token: hook.token.clone(),
                    },
                )
                .await?;
            mcp_configs.insert(node.key.clone(), path);
        }

        // 指纹在**执行之前**算好：它描述的是输入条件，跟结果无关。
        // 放到结束时算的话，中途失败的 run 就没有指纹，也就无法当基线比较。
        let fingerprint =
            ai_task_core::drift::fingerprint(&version.spec, &version.rules_hash, &skill_names);
        self.store
            .start_run(
                run_id,
                None,
                Some(&fingerprint),
                PendingEvent::run(RunEventBody::RunStarted {
                    worker: self.worker_id.clone(),
                }),
            )
            .await?;

        let sink = Mutex::new(EventSink::new(self.store.clone(), run_id));
        // dyn 化只在传给编排层时做，flush 仍然走具体类型
        let writer: &Mutex<dyn EventWriter> = &sink;
        let runner = ClaudeNodeRunner {
            executor: Arc::clone(&self.executor),
            sink: writer,
            resource_exceeded: std::sync::atomic::AtomicBool::new(false),
            env: NodeEnv {
                workdir: workdir.path(),
                system_append: system_append.as_deref(),
                settings_path: settings_path.as_deref(),
                skills: &materialized,
                store: &self.store,
                host_exec: self.host_exec.as_ref(),
                mcp_configs: &mcp_configs,
                workspace_id,
                run_id,
                dry_run: run.dry_run,
            },
        };

        let outcome =
            crate::dag::run_dag(&dag, &runner, writer, run.inputs.as_ref(), cancel).await?;
        sink.lock().await.flush().await?;

        let terminal = if outcome.cancelled || cancel.is_cancelled() {
            RunStatus::Cancelled
        } else if outcome.budget_exceeded {
            // 预算烧穿要和普通失败分开：前者是"钱不够"，改预算或拆任务；
            // 后者是任务本身出了问题。混在一起就看不出该动哪个旋钮。
            RunStatus::BudgetExceeded
        } else if runner
            .resource_exceeded
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            // 资源击穿要和普通失败分开：前者是"机器扛不住"，
            // 得去调 limits 或换机器；后者是任务本身写错了
            RunStatus::ResourceExceeded
        } else if outcome.failure.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Succeeded
        };

        // run 的输出取拓扑序里最后一个成功节点的输出——那是"最终结果"最自然的定义
        let final_output = dag
            .topo_order()
            .iter()
            .rev()
            .find_map(|key| outcome.outputs.get(key, 0).cloned());
        let digest = ai_task_core::drift::output_digest(final_output.as_ref());

        self.store
            .finish_run(
                run_id,
                RunOutcome {
                    status: terminal,
                    cost: outcome.total_cost,
                    output: final_output.clone(),
                    error: outcome.failure.clone(),
                    cli_version: outcome.cli_version,
                    output_digest: digest.clone(),
                },
                PendingEvent::run(RunEventBody::RunFinished {
                    status: terminal,
                    error: outcome.failure,
                    cost_usd: outcome.total_cost,
                }),
            )
            .await?;

        // 漂移检出放在终态**之后**：它要用刚写进去的 output_digest，
        // 而且检不出来也不该影响 run 的结论。
        // 传本次算出来的 fingerprint，**不要**用 `run.fingerprint`：那条记录是
        // 执行开始前读的，那时指纹列还是空的，比较会被静默跳过。
        self.flag_drift(
            workspace_id,
            run_id,
            &run,
            &fingerprint,
            digest.as_deref(),
            final_output.as_ref(),
        )
        .await;
        Ok(())
    }

    /// 和上一个同指纹的成功 run 比一比，真有漂移才发事件。
    ///
    /// **主动发，而不是等人来问。**一个每天自己跑的任务悄悄变了样，
    /// 如果只有点进详情页才看得到，那等于没有检测。
    async fn flag_drift(
        &self,
        workspace_id: WorkspaceId,
        run_id: RunId,
        run: &ai_task_store::RunRecord,
        fingerprint: &str,
        digest: Option<&str>,
        output: Option<&serde_json::Value>,
    ) {
        if digest.is_none() {
            return;
        }
        let baseline = match run.compare_to {
            Some(explicit) => self.store.get_run(workspace_id, explicit).await.ok(),
            None => self
                .store
                .previous_comparable_run(workspace_id, run.task_id, run_id, Some(fingerprint))
                .await
                .ok()
                .flatten(),
        };
        let Some(baseline) = baseline else { return };

        let drift = ai_task_core::drift::compare(
            baseline.fingerprint.as_deref(),
            baseline.output.as_ref(),
            Some(fingerprint),
            output,
        );
        if !drift.is_alarming() {
            return;
        }

        let changed_paths: Vec<String> = drift
            .changes
            .iter()
            .take(20)
            .map(|c| c.path.clone())
            .collect();
        tracing::warn!(
            %run_id, baseline = %baseline.id, fields = ?changed_paths,
            "检出行为漂移：输入条件没变，输出变了"
        );
        if let Err(err) = self
            .store
            .append_events(
                run_id,
                &[PendingEvent::run(RunEventBody::DriftDetected {
                    baseline_run_id: baseline.id,
                    changed_paths,
                })],
            )
            .await
        {
            tracing::warn!(%run_id, error = %err, "写入漂移事件失败");
        }
    }
}

/// 把 DAG 编排要跑的节点分派到具体执行器。
struct ClaudeNodeRunner<'a> {
    executor: Arc<dyn Executor>,
    sink: &'a Mutex<dyn EventWriter>,
    env: NodeEnv<'a>,
    /// 任一节点被 cgroup 限额打死过。整个 run 因此落 `resource_exceeded`。
    resource_exceeded: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl NodeRunner for ClaudeNodeRunner<'_> {
    async fn run(&self, ctx: NodeRunContext<'_>) -> NodeResult {
        match &ctx.node.config {
            NodeConfig::Ai(ai) => self.run_ai(ai, &ctx).await,
            NodeConfig::Shell(shell) => self.run_shell(shell, &ctx).await,
            NodeConfig::Approval(approval) => self.run_approval(approval, &ctx).await,
            NodeConfig::Assert(assert) => match crate::dag::eval_assert(assert, &ctx.inputs) {
                // 编排写错了（缺输入、judge 未实现）：这是 spec 的问题，
                // 重试多少次都一样，直接失败并说清原因
                Err(spec_error) => NodeResult::failed(spec_error),
                Ok(Ok(())) => NodeResult::succeeded(Some(serde_json::json!({"passed": true}))),
                Ok(Err(reason)) => NodeResult::failed(reason),
            },
            // 明确报"未实现"，而不是假装成功——后者会让用户以为节点跑过了
            other => NodeResult::failed(format!("节点类型 `{}` 尚未实现", other.kind())),
        }
    }
}

impl ClaudeNodeRunner<'_> {
    /// 审批门：挂起，直到有人点头或者到点。
    ///
    /// **默认动作是拒绝。**超时放行会把审批门变成一个延迟 15 分钟的空操作，
    /// 而配审批门的人恰恰是不想让它自动通过的人。
    async fn run_approval(
        &self,
        approval: &ai_task_proto::ApprovalNode,
        ctx: &NodeRunContext<'_>,
    ) -> NodeResult {
        let node = ctx.node;

        if suppressed_by_dry_run(self.env.dry_run, &node.config) {
            let _ = self
                .sink
                .lock()
                .await
                .node(
                    &node.key,
                    RunEventBody::Log {
                        level: LogLevel::Info,
                        message: format!("影子执行：跳过审批门「{}」", approval.title),
                    },
                )
                .await;
            return NodeResult::succeeded(Some(serde_json::json!({
                "dry_run": true,
                "approved": false,
                "skipped": true,
            })));
        }

        let expires_at = chrono::Utc::now()
            + chrono::Duration::seconds(i64::from(approval.timeout_s).clamp(1, 24 * 3600));
        let intent = build_intent(node, ctx);

        let requested = crate::approval::request_and_wait(
            self.env.store,
            ai_task_store::NewApproval {
                run_id: self.env.run_id,
                node_key: Some(node.key.clone()),
                title: approval.title.clone(),
                intent: intent.clone(),
                rule_id: None,
                expires_at,
            },
            ctx.cancel,
        )
        .await;

        let (approval_id, verdict) = match requested {
            Ok(pair) => pair,
            // 审批建不出来就没有门。宁可让节点失败，也不能当作通过。
            Err(err) => return NodeResult::failed(format!("创建审批失败，按未批准处理：{err}")),
        };

        // 事件在**等待开始之后**才写：等待期间界面靠 pending_approvals 接口
        // 拿卡片，这里补齐的是审计流水。两条一起写，顺序才对得上。
        let mut sink = self.sink.lock().await;
        let _ = sink
            .node(
                &node.key,
                RunEventBody::ApprovalRequested {
                    approval_id,
                    title: approval.title.clone(),
                    intent,
                    expires_at,
                },
            )
            .await;
        let (approved, by, reason) = match &verdict {
            crate::approval::Verdict::Approved { by, reason } => (true, by.clone(), reason.clone()),
            crate::approval::Verdict::Denied { by, reason } => (false, by.clone(), reason.clone()),
            other => (false, None, Some(other.describe())),
        };
        let _ = sink
            .node(
                &node.key,
                RunEventBody::ApprovalDecided {
                    approval_id,
                    approved,
                    decided_by: by.clone(),
                    reason: reason.clone(),
                },
            )
            .await;
        drop(sink);

        if matches!(verdict, crate::approval::Verdict::Cancelled) {
            return NodeResult {
                status: NodeStatus::Cancelled,
                output: None,
                error: Some(verdict.describe()),
                cost: UsdMicros::ZERO,
                cli_version: None,
            };
        }
        if verdict.is_approved() {
            NodeResult::succeeded(Some(serde_json::json!({
                "approved": true,
                "decided_by": by,
                "reason": reason,
            })))
        } else {
            NodeResult::failed(verdict.describe())
        }
    }

    /// shell 节点：把命令交给目标机上的 agent。
    ///
    /// 本机也走 agent，不走 `Command::spawn`。多一次进程跳转换来的是
    /// **本机执行同样有资源归因和上限**，以及只有一条代码路径要维护。
    async fn run_shell(
        &self,
        shell: &ai_task_proto::ShellNode,
        ctx: &NodeRunContext<'_>,
    ) -> NodeResult {
        let Some(config) = self.env.host_exec else {
            return NodeResult::failed(
                "这个部署没有配置 agent 二进制，shell 节点不可用（见 --local-agent / --remote-agent）",
            );
        };
        let node = ctx.node;

        if suppressed_by_dry_run(self.env.dry_run, &node.config) {
            let _ = self
                .sink
                .lock()
                .await
                .node(
                    &node.key,
                    RunEventBody::Log {
                        level: LogLevel::Info,
                        message: format!("影子执行：不会真的运行 `{}`", shell.command),
                    },
                )
                .await;
            return NodeResult::succeeded(Some(serde_json::json!({
                "dry_run": true,
                "executed": false,
                "command": shell.command,
                "host": node.host,
            })));
        }

        // 默认工作目录取决于落在哪台机器上。**中心的 run workdir 在远端不存在**，
        // 拿它当远端的 cwd 会让每个远端 shell 节点都以 ENOENT 起手。
        // 远端默认 "."：SSH exec channel 的 cwd 就是登录用户的家目录。
        let remote = matches!(node.host, Some(ai_task_proto::HostSelector::Host { .. }));
        let cwd = shell.working_dir.clone().unwrap_or_else(|| {
            if remote {
                ".".to_owned()
            } else {
                self.env.workdir.display().to_string()
            }
        });
        // 文件操作的根就是命令的工作目录。
        // 空 roots 等于禁止所有文件操作，不是"不限制"。
        let roots = vec![cwd.clone()];

        let timeout_ms = node
            .timeout_s
            .map_or(30 * 60 * 1000, |s| u64::from(s).saturating_mul(1000));

        let started = std::time::Instant::now();
        let outcome = tokio::select! {
            () = ctx.cancel.cancelled() => {
                return NodeResult {
                    status: NodeStatus::Cancelled,
                    output: None,
                    error: Some("run 已取消".into()),
                    cost: UsdMicros::ZERO,
                    cli_version: None,
                };
            }
            result = crate::host_exec::run_command(
                self.env.store,
                config,
                crate::host_exec::Command {
                    workspace_id: self.env.workspace_id,
                    run_id: self.env.run_id,
                    node_key: node.key.as_str(),
                    selector: node.host.as_ref(),
                    command: &shell.command,
                    cwd: &cwd,
                    timeout_ms,
                    limits: node.limits,
                    roots: &roots,
                },
            ) => result,
        };

        let outcome = match outcome {
            Ok(outcome) => outcome,
            // 连不上目标机 / 推不动 agent：这是环境问题，重试有意义，
            // 所以照常返回 Failed 让 retry 策略去决定
            Err(err) => return NodeResult::failed(format!("目标机执行失败：{err}")),
        };

        if outcome.cgroup_mode != ai_task_agent::protocol::CgroupMode::Systemd && ctx.attempt == 1 {
            // 降级要留痕：界面上据此标注"该节点的资源数据不完整/上限未强制"
            let _ = self
                .sink
                .lock()
                .await
                .node(
                    &node.key,
                    RunEventBody::ResourceDegraded {
                        mode: format!("{:?}", outcome.cgroup_mode).to_lowercase(),
                        detail: outcome.cgroup_detail.clone(),
                    },
                )
                .await;
        }

        if crate::host_exec::is_resource_exceeded(&outcome) {
            self.resource_exceeded
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }

        tracing::debug!(
            node = %node.key,
            elapsed_ms = started.elapsed().as_millis(),
            samples = outcome.samples,
            "shell 节点执行结束"
        );
        crate::host_exec::to_node_result(&outcome)
    }

    async fn run_ai(&self, ai: &ai_task_proto::AiNode, ctx: &NodeRunContext<'_>) -> NodeResult {
        let node = ctx.node;
        if !self.env.skills.is_empty() && ctx.attempt == 1 && ctx.instance == 0 {
            let _ = self
                .sink
                .lock()
                .await
                .node(
                    &node.key,
                    RunEventBody::Log {
                        level: LogLevel::Info,
                        message: format!("已装载 skills：{}", self.env.skills.join(", ")),
                    },
                )
                .await;
        }

        let prompt = compose_prompt(&ai.prompt, ctx);

        // 远端节点：只给远端工具，**本地的 Bash/Read/Write 一律去掉**。
        // 留着的话模型会在中心机上老老实实执行本地工具——而它以为自己
        // 操作的是目标机。这类错误没有任何报错，只有做错了的事。
        let mcp_config = self.env.mcp_configs.get(&node.key);
        let tools = if mcp_config.is_some() {
            let mut tools = ai_task_exec::remote_tool_names();
            // Task 留着：它碰不到文件系统，子 agent 继承同一份工具白名单。
            // 不往里塞 CLI 不认识的名字——被静默忽略的白名单项是最难查的一类配置错误。
            tools.push("Task".to_owned());
            Some(tools)
        } else {
            (!ai.tools.is_empty()).then(|| ai.tools.clone())
        };
        let handle = match self
            .executor
            .spawn(ExecRequest {
                node_key: node.key.clone(),
                prompt,
                workdir: self.env.workdir.to_path_buf(),
                model: ai.model.clone(),
                effort: ai.effort,
                tools,
                output_schema: node.output_schema.clone(),
                budget_usd: ai.budget_usd,
                system_append: self.env.system_append.map(str::to_owned),
                session_id: uuid::Uuid::now_v7(),
                settings_path: self.env.settings_path.map(std::path::Path::to_path_buf),
                has_skills: !self.env.skills.is_empty(),
                mcp_config: mcp_config.cloned(),
            })
            .await
        {
            Ok(handle) => handle,
            Err(err) => return NodeResult::failed(err.to_string()),
        };

        let mut events = handle.events;
        let exec_cancel = handle.cancel;
        let mut cost = UsdMicros::ZERO;
        let mut cli_version = None;
        let mut outcome = ExecOutcome::Failed {
            reason: "执行器没有给出终态".into(),
        };

        // 取消只处理一次：`cancelled()` 一旦就绪就永远立刻返回，配上 `biased`
        // 会把事件分支饿死，表现是 100% CPU 空转。
        let mut cancel_signalled = false;
        let mut cancel_deadline: Option<tokio::time::Instant> = None;

        loop {
            tokio::select! {
                biased;
                () = ctx.cancel.cancelled(), if !cancel_signalled => {
                    cancel_signalled = true;
                    cancel_deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
                    exec_cancel.cancel();
                }
                () = sleep_until_opt(cancel_deadline), if cancel_deadline.is_some() => {
                    tracing::warn!(node = %node.key, "执行器在取消后未按时收尾，强制结束");
                    outcome = ExecOutcome::Cancelled;
                    break;
                }
                event = events.recv() => {
                    let Some(event) = event else { break };
                    match &event {
                        ExecEvent::Finished(finished) => outcome = finished.clone(),
                        ExecEvent::Usage { cost_usd, .. } => cost = *cost_usd,
                        ExecEvent::Started { cli_version: version, .. } => {
                            cli_version.clone_from(version);
                        }
                        _ => {}
                    }
                    let mut sink = self.sink.lock().await;
                    for body in translate(&event) {
                        if sink.node(&node.key, body).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }

        let (status, output, error) = match &outcome {
            ExecOutcome::Success { result, .. } => {
                (NodeStatus::Succeeded, parse_output(result), None)
            }
            ExecOutcome::Failed { reason } => (NodeStatus::Failed, None, Some(reason.clone())),
            ExecOutcome::BudgetExceeded => (
                NodeStatus::Failed,
                None,
                Some(format!("成本超出节点预算（已花 ${cost}）")),
            ),
            ExecOutcome::Cancelled => (NodeStatus::Cancelled, None, None),
        };

        NodeResult {
            status,
            output,
            error,
            cost,
            cli_version,
        }
    }
}

/// 把已解析的输入、map 元素、上一次的失败原因拼进提示词。
fn compose_prompt(base: &str, ctx: &NodeRunContext<'_>) -> String {
    let mut prompt = base.to_string();

    if let Some(item) = ctx.map_item {
        prompt.push_str(&format!(
            "\n\n## 本次处理的元素\n\n```json\n{}\n```",
            serde_json::to_string_pretty(item).unwrap_or_else(|_| item.to_string())
        ));
    }

    if !ctx.inputs.is_empty() {
        prompt.push_str("\n\n## 上游输入\n");
        for (name, value) in &ctx.inputs {
            prompt.push_str(&format!(
                "\n### {name}\n\n```json\n{}\n```\n",
                serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
            ));
        }
    }

    // 回喂放在最后：模型对靠近结尾的指令更敏感，而这正是本轮最要紧的信息
    if let Some(feedback) = ctx.feedback {
        prompt.push_str(&format!(
            "\n\n## 上一次尝试失败了\n\n{feedback}\n\n             请针对这个原因调整做法，不要原样重来。",
        ));
    }
    prompt
}

/// 一个节点执行时的环境。
#[derive(Clone, Copy)]
struct NodeEnv<'a> {
    workdir: &'a std::path::Path,
    /// 合成后的软规则文本。
    system_append: Option<&'a str>,
    /// 策略 hook 的 settings 文件。
    settings_path: Option<&'a std::path::Path>,
    /// 已落地的技能名。
    skills: &'a [String],
    store: &'a Store,
    /// `None` 表示这个部署没准备 agent，shell 节点不可用。
    host_exec: Option<&'a crate::host_exec::HostExecConfig>,
    /// node key → 该节点的 `--mcp-config` 文件。不在表里就是不给远端工具。
    mcp_configs: &'a BTreeMap<NodeKey, std::path::PathBuf>,
    workspace_id: WorkspaceId,
    run_id: RunId,
    /// 影子执行：副作用只记录意图，不真的做。
    dry_run: bool,
}

/// 把软规则合成一段注入 system prompt 的文本。
///
/// 按优先级排好并逐条编号：模型对有结构的清单遵守得更好，而且事故复盘时
/// 能直接对上是哪一条。返回 `None` 表示没有规则，此时不该占一个 CLI 参数。
fn compose_rules(rules: &[ai_task_store::PromptRule]) -> Option<String> {
    let lines: Vec<String> = rules
        .iter()
        .filter(|r| !r.text.trim().is_empty())
        .enumerate()
        .map(|(i, r)| format!("{}. [{}] {}", i + 1, r.name, r.text.trim()))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "以下是本次执行必须遵守的规则，按优先级排列：\n{}\n\n\
         这些是软约束。另有一层硬策略在工具调用边界强制执行，\n\
         被它拒绝时会收到明确原因，请据此调整做法，不要反复重试同一个动作。",
        lines.join("\n")
    ))
}

/// 收集 DAG 里所有 AI 节点声明的 skill 名（含 map 模板内部）。
fn collect_skill_names(dag: &ValidatedDag) -> Vec<String> {
    fn walk(node: &NodeSpec, out: &mut Vec<String>) {
        if let NodeConfig::Ai(ai) = &node.config {
            out.extend(ai.skills.iter().cloned());
        }
        if let NodeConfig::Map(m) = &node.config {
            walk(&m.template, out);
        }
    }
    let mut names = Vec::new();
    for node in &dag.spec().nodes {
        walk(node, &mut names);
    }
    names.sort();
    names.dedup();
    names
}

/// 声明了 `output_schema` 时模型产出的是一段 JSON 文本；没声明时是自由文本。
/// 两种都要能落进 `output` 这个 jsonb 列。
fn parse_output(result: &str) -> Option<serde_json::Value> {
    if result.is_empty() {
        return None;
    }
    Some(
        serde_json::from_str(result)
            .unwrap_or_else(|_| serde_json::Value::String(result.to_string())),
    )
}

/// 执行器事件 → run 事件。
///
/// 这里刻意**不**发 `AgentTurnStarted`：CLI 的流里没有可靠的轮次边界，
/// `num_turns` 只在终态才有。编一个出来会让时间轴显示错误的轮次划分。
fn translate(event: &ExecEvent) -> Vec<RunEventBody> {
    match event {
        ExecEvent::Started {
            session_id,
            model,
            tools,
            cli_version,
        } => vec![RunEventBody::Log {
            level: LogLevel::Info,
            message: format!(
                "执行器就绪：model={model} session={session_id} cli={} tools=[{}]",
                cli_version.as_deref().unwrap_or("?"),
                tools.join(",")
            ),
        }],
        ExecEvent::Thinking { text } => vec![RunEventBody::AgentThinking { text: text.clone() }],
        ExecEvent::Text { text } => vec![RunEventBody::AgentText { text: text.clone() }],
        ExecEvent::ToolRequested {
            tool_use_id,
            tool,
            input,
        } => vec![RunEventBody::ToolRequested {
            tool_use_id: tool_use_id.clone(),
            tool: tool.clone(),
            input: input.clone(),
        }],
        ExecEvent::ToolCompleted {
            tool_use_id,
            outcome,
            output_preview,
        } => vec![RunEventBody::ToolCompleted {
            tool_use_id: tool_use_id.clone(),
            ok: *outcome == ToolOutcome::Ok,
            output_preview: output_preview.clone(),
            duration_ms: 0,
        }],
        // CLI 自己的权限层拒绝也要进审计。M2 起我们的策略层会在它之前先判一道，
        // 但两者的判决都落在同一种事件上，界面不用区分。
        ExecEvent::PermissionDenied {
            tool_use_id,
            reason,
            ..
        } => vec![RunEventBody::PolicyDecided {
            tool_use_id: tool_use_id.clone(),
            effect: PolicyEffect::Deny,
            rule_id: None,
            reason: reason.clone(),
        }],
        ExecEvent::Usage {
            model,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            cost_usd,
        } => vec![RunEventBody::Usage {
            model: model.clone(),
            input_tokens: *input_tokens,
            output_tokens: *output_tokens,
            cache_read_tokens: *cache_read_tokens,
            cache_creation_tokens: *cache_creation_tokens,
            cost_usd: *cost_usd,
        }],
        ExecEvent::Warning { message } => vec![RunEventBody::Log {
            level: LogLevel::Warn,
            message: message.clone(),
        }],
        // 终态由调用方转成 NodeFinished，它才知道 attempt 是第几次
        ExecEvent::Finished(_) => Vec::new(),
    }
}

/// 引擎重启后，把因为进程消失而中断的 run 收尾。
///
/// **不尝试续跑**：CLI 子进程随服务一起没了，上下文不在了。状态本身没丢
/// （事件日志还在，[`RunState::replay`] 能完整重建），但执行没法接着走。
/// 与其留一堆永远停在 running 的 run，不如明确标记失败让人能重跑。
pub async fn reap_orphaned_runs(store: &Store, worker_id: &str) -> Result<usize, StoreError> {
    let orphans = store.unfinished_runs(500).await?;
    let mut reaped = 0;
    for run in orphans {
        let events = store.read_events_after(run.id, 0, 100_000).await?;
        // 先重放，把已经花掉的成本捞回来——否则这些钱在账上就消失了
        let cost = match RunState::replay(&events) {
            Ok(state) => state.cost,
            Err(err) => {
                tracing::warn!(run_id = %run.id, error = %err, "事件流重放失败，成本按 0 记");
                run.cost
            }
        };
        let reason = format!("{worker_id} 重启时发现这个 run 的执行进程已消失，无法续跑");
        store
            .finish_run(
                run.id,
                RunOutcome::new(RunStatus::Failed, cost).with_error(reason.clone()),
                PendingEvent::run(RunEventBody::RunFinished {
                    status: RunStatus::Failed,
                    error: Some(reason),
                    cost_usd: cost,
                }),
            )
            .await?;
        reaped += 1;
    }
    if reaped > 0 {
        tracing::warn!(reaped, "已收尾中断的 run");
    }
    Ok(reaped)
}

/// 审批卡片要展示的结构化意图。
///
/// **不是一段自然语言。**要让人在几秒内判断"这该不该做"，卡片上必须是
/// 具体的：哪台机器、跑什么命令、改哪些文件。一段"我将执行一些维护操作"
/// 只会让审批退化成无脑点通过。
fn build_intent(node: &ai_task_proto::NodeSpec, ctx: &NodeRunContext<'_>) -> serde_json::Value {
    let mut intent = serde_json::Map::new();
    intent.insert("node".into(), serde_json::json!(node.key.as_str()));
    if let Some(host) = &node.host {
        intent.insert(
            "host".into(),
            serde_json::to_value(host).unwrap_or_default(),
        );
    }
    // 上游输出就是"接下来要拿它去做什么"的全部依据
    if !ctx.inputs.is_empty() {
        intent.insert(
            "inputs".into(),
            serde_json::Value::Object(ctx.inputs.clone().into_iter().collect()),
        );
    }
    if let Some(item) = ctx.map_item {
        intent.insert("map_item".into(), item.clone());
    }
    serde_json::Value::Object(intent)
}

/// 影子执行下，这个节点该不该被替换成"只记录意图不执行"。
///
/// `dry_run` 本身是在**策略层**实现的，而策略层只看得见模型发起的工具调用。
/// shell 和 approval 由引擎直接执行，绕开了那一层——实测撞到过：
/// 一个 dry_run 的 run 真的在目标机上 touch 出了文件。
///
/// 穷举匹配是刻意的：新增一个节点类型时，编译器会逼着在这里做出选择。
#[must_use]
fn suppressed_by_dry_run(dry_run: bool, config: &NodeConfig) -> bool {
    if !dry_run {
        return false;
    }
    match config {
        // 引擎直接执行，会真的改动目标机
        NodeConfig::Shell(_) => true,
        // 把人叫起来点一个根本不会发生的操作，是浪费他的时间
        NodeConfig::Approval(_) => true,
        // AI 节点照常跑：影子执行要的就是"看看模型这次会得出什么结论"。
        // 它的副作用由策略层按工具逐个拦。
        NodeConfig::Ai(_) => false,
        // 纯计算，没有外部副作用
        NodeConfig::Assert(_) | NodeConfig::Map(_) => false,
    }
}

/// 这个节点需不需要远端工具。
///
/// 只有**声明了远端主机的 AI 节点**需要：shell 节点走 agent 直连，
/// assert / approval 根本不碰目标机。
fn needs_remote_tools(node: &ai_task_proto::NodeSpec) -> bool {
    matches!(node.config, NodeConfig::Ai(_))
        && matches!(node.host, Some(ai_task_proto::HostSelector::Host { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shadow_run_touches_nothing_outside_the_model() {
        use ai_task_proto::{ApprovalNode, ApprovalTimeout, ShellNode};

        let shell = NodeConfig::Shell(ShellNode {
            command: "rm -rf /".into(),
            working_dir: None,
        });
        let gate = NodeConfig::Approval(ApprovalNode {
            title: "确认".into(),
            timeout_s: 60,
            on_timeout: ApprovalTimeout::Deny,
        });

        // 影子执行下，会动到外界的节点一个都不能真跑
        assert!(suppressed_by_dry_run(true, &shell));
        assert!(suppressed_by_dry_run(true, &gate));

        // 非影子执行下一律照跑
        assert!(!suppressed_by_dry_run(false, &shell));
        assert!(!suppressed_by_dry_run(false, &gate));
    }

    #[test]
    fn a_shadow_run_still_asks_the_model() {
        // 影子执行要的就是"看看模型这次会得出什么结论"。
        // 把 AI 节点也停掉的话，就没有输出可以和基线比了。
        let ai = NodeConfig::Ai(ai_task_proto::AiNode {
            prompt: "x".into(),
            executor: ai_task_proto::ExecutorKind::ClaudeCode,
            model: None,
            effort: None,
            skills: vec![],
            tools: vec![],
            max_turns: None,
            budget_usd: None,
        });
        assert!(!suppressed_by_dry_run(true, &ai));
    }

    #[test]
    fn a_remote_ai_node_loses_every_local_filesystem_tool() {
        // 这是整个远端执行里最容易出、也最难查的错：本地工具还留着的话，
        // 模型会在**中心机**上乖乖执行 Bash/Write，而它以为自己在操作目标机。
        // 没有报错，只有做错了的事。
        let remote = ai_task_exec::remote_tool_names();
        assert!(
            remote
                .iter()
                .all(|t| t.starts_with("mcp__ai_task_remote__"))
        );
        for local in [
            "Bash",
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Grep",
            "NotebookEdit",
        ] {
            assert!(
                !remote.iter().any(|t| t == local),
                "远端工具集里混进了本地工具 {local}"
            );
        }
    }

    #[test]
    fn only_ai_nodes_pinned_to_a_host_need_the_remote_proxy() {
        use ai_task_proto::{
            AiNode, ExecutorKind, HostId, HostSelector, NodeSpec, OnFailure, RetryPolicy, ShellNode,
        };

        let mut node = NodeSpec {
            key: NodeKey::parse("n").expect("合法 key"),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: "x".into(),
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
        };
        assert!(!needs_remote_tools(&node), "没声明主机的 AI 节点不需要代理");

        node.host = Some(HostSelector::Local);
        assert!(!needs_remote_tools(&node), "本机节点不需要远端代理");

        node.host = Some(HostSelector::Host {
            host_id: HostId::new(),
        });
        assert!(needs_remote_tools(&node));

        // shell 节点直连 agent，不经过 MCP——给它挂代理是多一条无人走的路径
        node.config = NodeConfig::Shell(ShellNode {
            command: "ls".into(),
            working_dir: None,
        });
        assert!(!needs_remote_tools(&node));
    }

    #[test]
    fn free_text_and_json_output_both_land_in_jsonb() {
        assert_eq!(
            parse_output(r#"{"lines": 4}"#),
            Some(serde_json::json!({"lines": 4}))
        );
        // 没声明 schema 时模型给的是自由文本，也要能存
        assert_eq!(
            parse_output("4"),
            Some(serde_json::json!(4)),
            "裸数字是合法 JSON"
        );
        assert_eq!(
            parse_output("一共 4 行"),
            Some(serde_json::json!("一共 4 行"))
        );
        assert_eq!(parse_output(""), None);
    }

    #[test]
    fn terminal_exec_events_do_not_translate_to_run_events() {
        // 终态得由调用方转成 NodeFinished，它才知道 attempt
        assert!(translate(&ExecEvent::Finished(ExecOutcome::Cancelled)).is_empty());
    }

    #[test]
    fn cli_permission_denials_land_in_the_audit_trail() {
        let events = translate(&ExecEvent::PermissionDenied {
            tool_use_id: "t1".into(),
            tool: "Bash".into(),
            reason: "Contains command_substitution".into(),
        });
        let [RunEventBody::PolicyDecided { effect, reason, .. }] = events.as_slice() else {
            panic!("权限拒绝必须落成策略判决：{events:?}");
        };
        assert_eq!(*effect, PolicyEffect::Deny);
        assert_eq!(reason, "Contains command_substitution");
    }
}
