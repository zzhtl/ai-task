//! DAG 执行编排。
//!
//! 与 M1 的「按拓扑序顺序跑一遍」相比，这里多了四件事：条件边、重试、
//! `map` 动态展开、输出契约校验。
//!
//! 两个刻意的设计选择：
//!
//! - **`map` 是节点内部的扇出，不是图重写。** 展开出来的实例在事件日志和
//!   `run_nodes` 里各占一行（靠 `instance` 区分），但**图本身不变**。
//!   下游像读普通节点一样读 map 节点的输出（一个数组）。图在运行时变形会让
//!   拓扑序、就绪判定、取消传播全部变成动态问题，不值得。
//! - **就绪判定看入边而不是看拓扑序。** 一个节点只有在它**所有**入边都有了
//!   结论（放行或否决）之后才能判断；至少一条放行才执行，全部否决就跳过。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ai_task_core::ValidatedDag;
use ai_task_core::eval::{EvalContext, NodeOutputs, eval_condition, resolve, validate_output};
use ai_task_proto::{
    AssertNode, Cmp, LogLevel, NodeConfig, NodeKey, NodeSpec, NodeStatus, RunEventBody, UsdMicros,
};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::sink::EventWriter;

/// 单个节点实例的执行结果。
#[derive(Debug, Clone)]
pub struct NodeResult {
    pub status: NodeStatus,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub cost: UsdMicros,
    pub cli_version: Option<String>,
}

impl NodeResult {
    #[must_use]
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: NodeStatus::Failed,
            output: None,
            error: Some(error.into()),
            cost: UsdMicros::ZERO,
            cli_version: None,
        }
    }

    #[must_use]
    pub fn succeeded(output: Option<Value>) -> Self {
        Self {
            status: NodeStatus::Succeeded,
            output,
            error: None,
            cost: UsdMicros::ZERO,
            cli_version: None,
        }
    }
}

/// 一次 DAG 执行的产物。
#[derive(Debug, Default)]
pub struct DagOutcome {
    pub outputs: NodeOutputs,
    pub total_cost: UsdMicros,
    pub cli_version: Option<String>,
    /// 第一个失败节点的错误。`None` 表示整个 DAG 跑通了。
    pub failure: Option<String>,
    pub cancelled: bool,
    /// 预算烧穿，剩下的节点没有启动。
    pub budget_exceeded: bool,
}

/// 节点执行器：把一个具体节点跑起来。
///
/// 抽成 trait 是为了让 DAG 编排能在**不碰真实执行器**的情况下测试
/// ——条件边、重试、map 展开这些逻辑的 bug，不该靠烧 API 额度去发现。
#[async_trait::async_trait]
pub trait NodeRunner: Send + Sync {
    /// 跑一个节点实例。
    ///
    /// `attempt` 从 1 开始。`feedback` 是上一次失败的原因（仅当
    /// `retry.feed_error_to_model` 为真时给出）。
    async fn run(&self, ctx: NodeRunContext<'_>) -> NodeResult;
}

/// 一次节点执行的入参。
pub struct NodeRunContext<'a> {
    pub node: &'a NodeSpec,
    /// `map` 展开出来的第几个实例；非 map 节点恒为 0。
    pub instance: u32,
    pub attempt: u32,
    /// 上一次失败的原因，会被拼进提示词。
    pub feedback: Option<&'a str>,
    /// 已解析好的输入：名字 → 值。
    pub inputs: BTreeMap<String, Value>,
    /// map 模板节点的当前元素。
    pub map_item: Option<&'a Value>,
    pub cancel: &'a CancellationToken,
}

/// 跑完整个 DAG。
pub async fn run_dag(
    dag: &ValidatedDag,
    runner: &dyn NodeRunner,
    sink: &Mutex<dyn EventWriter>,
    run_inputs: Option<&Value>,
    cancel: &CancellationToken,
) -> Result<DagOutcome, ai_task_store::StoreError> {
    let mut state = RunState::new(dag);
    let mut outcome = DagOutcome::default();

    loop {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            state.cancel_pending(sink, &mut outcome).await?;
            break;
        }

        let Ready { ready, skipped } = state.take_ready(dag, &outcome.outputs, run_inputs);

        // 因为入边条件全部否决而跳过的节点也要留痕。不发事件的话它在界面上
        // 会永远停在 pending，事件重放也不会把它标成终态。
        for (key, reason) in &skipped {
            sink.lock()
                .await
                .node(
                    key,
                    RunEventBody::NodeSkipped {
                        reason: reason.clone(),
                    },
                )
                .await?;
        }

        if ready.is_empty() {
            if skipped.is_empty() {
                break;
            }
            // 这一轮只跳过了节点，可能让下一批变得可判定，接着转
            continue;
        }

        // 同一批就绪的节点并发跑。上限来自 DAG 里最保守的那个 max_parallel，
        // 免得一个宽松的节点把整个 run 的并发度拉上去。
        let mut futures = Vec::with_capacity(ready.len());
        for key in &ready {
            let node = dag.node(key).expect("就绪集里的节点必然存在");
            futures.push(run_one_node(
                node,
                runner,
                sink,
                &outcome.outputs,
                run_inputs,
                cancel,
            ));
        }
        let results = futures_util::future::join_all(futures).await;

        for (key, result) in ready.iter().zip(results) {
            let result = result?;
            outcome.total_cost = outcome.total_cost.saturating_add(result.cost);
            if result.cli_version.is_some() {
                outcome.cli_version.clone_from(&result.cli_version);
            }
            if let Some(output) = &result.output {
                // map 节点的输出是各实例结果的数组，仍然按单实例存
                outcome.outputs.insert(key.clone(), 0, output.clone());
            }
            if result.status == NodeStatus::Failed && outcome.failure.is_none() {
                outcome.failure.clone_from(&result.error);
            }
            state.finish(key.clone(), result.status);
        }

        // 预算熔断：在**批与批之间**检查。
        //
        // 节点级的 `--max-budget-usd` 管的是单个节点，管不住"十个便宜节点加起来
        // 超了"。这里是 run 级的闸门：已经花掉的钱拦不住（那笔钱真的花了），
        // 但可以不再启动新的节点。
        if let Some(budget) = dag.spec().budget_usd
            && outcome.total_cost >= budget
        {
            outcome.budget_exceeded = true;
            let spent = outcome.total_cost.to_decimal_string();
            let cap = budget.to_decimal_string();
            sink.lock()
                .await
                .run(RunEventBody::Log {
                    level: LogLevel::Warn,
                    message: format!("已花费 ${spent} 达到预算上限 ${cap}，不再启动新节点"),
                })
                .await?;
            outcome.failure.get_or_insert_with(|| {
                format!("预算 ${cap} 已用尽（实际花费 ${spent}），剩余节点未执行")
            });
            state.cancel_pending(sink, &mut outcome).await?;
            break;
        }

        // fail_fast：只要有节点最终失败，就不再启动新的节点。
        // 已经在跑的那一批会跑完——它们没有被取消，中途掐掉只会留下半截状态。
        if state.should_stop(dag) {
            state.cancel_pending(sink, &mut outcome).await?;
            break;
        }
    }

    Ok(outcome)
}

/// 跑一个节点（含重试、schema 校验、map 展开）。
async fn run_one_node(
    node: &NodeSpec,
    runner: &dyn NodeRunner,
    sink: &Mutex<dyn EventWriter>,
    outputs: &NodeOutputs,
    run_inputs: Option<&Value>,
    cancel: &CancellationToken,
) -> Result<NodeResult, ai_task_store::StoreError> {
    sink.lock()
        .await
        .node(&node.key, RunEventBody::NodeReady)
        .await?;

    let ctx = EvalContext {
        outputs,
        run_inputs,
        map_item: None,
    };
    let inputs = match resolve_inputs(node, &ctx) {
        Ok(inputs) => inputs,
        Err(err) => {
            let result = NodeResult::failed(err);
            emit_finished(sink, node, 0, 1, &result).await?;
            return Ok(result);
        }
    };

    // map 节点走扇出路径；其它节点按普通节点跑（含重试）
    if let NodeConfig::Map(map) = &node.config {
        return expand_map(node, map, runner, sink, &ctx, &inputs, cancel).await;
    }

    let result = attempt_with_retry(node, 0, runner, sink, &inputs, None, cancel).await?;
    Ok(result)
}

/// 带重试地跑一个节点实例。
async fn attempt_with_retry(
    node: &NodeSpec,
    instance: u32,
    runner: &dyn NodeRunner,
    sink: &Mutex<dyn EventWriter>,
    inputs: &BTreeMap<String, Value>,
    map_item: Option<&Value>,
    cancel: &CancellationToken,
) -> Result<NodeResult, ai_task_store::StoreError> {
    let max_attempts = node.retry.max_attempts.max(1);
    let mut feedback: Option<String> = None;
    let mut last = NodeResult::failed("节点从未产出结果");

    for attempt in 1..=max_attempts {
        if cancel.is_cancelled() {
            let cancelled = NodeResult {
                status: NodeStatus::Cancelled,
                output: None,
                error: None,
                cost: last.cost,
                cli_version: last.cli_version.clone(),
            };
            emit_finished(sink, node, instance, attempt, &cancelled).await?;
            return Ok(cancelled);
        }

        sink.lock()
            .await
            .node(
                &node.key,
                RunEventBody::NodeStarted {
                    attempt,
                    // 只有按 id 指定的主机能在这里确定下来。按 tag 选主机要等
                    // 到执行时才知道落到了哪台，那时补一条事件更诚实——
                    // 现在猜一个会让审计里出现"跑在了 A 上"的假记录。
                    host_id: match &node.host {
                        Some(ai_task_proto::HostSelector::Host { host_id }) => Some(*host_id),
                        _ => None,
                    },
                },
            )
            .await?;

        let mut result = runner
            .run(NodeRunContext {
                node,
                instance,
                attempt,
                feedback: feedback.as_deref(),
                inputs: inputs.clone(),
                map_item,
                cancel,
            })
            .await;

        // 输出契约校验。**成功才校验**——失败的节点没有输出可校验。
        if result.status == NodeStatus::Succeeded
            && let Some(schema) = &node.output_schema
        {
            let output = result.output.clone().unwrap_or(Value::Null);
            if let Err(errors) = validate_output(schema, &output) {
                result = NodeResult {
                    status: NodeStatus::Failed,
                    output: result.output,
                    error: Some(format!("输出不符合契约：{}", errors.join("；"))),
                    cost: result.cost,
                    cli_version: result.cli_version,
                };
            }
        }

        last = result.clone();

        if result.status != NodeStatus::Failed || attempt == max_attempts {
            emit_finished(sink, node, instance, attempt, &result).await?;
            return Ok(result);
        }

        // 还有重试机会：把失败原因记下来，按策略决定要不要回喂给模型。
        // 只有 AI 节点有模型可喂——在 shell/assert 上报 `feeding_error_to_model: true`
        // 是在审计日志里写一件没发生的事。
        let feeds_model = node.retry.feed_error_to_model
            && matches!(node.config, ai_task_proto::NodeConfig::Ai(_));
        let reason = result.error.clone().unwrap_or_else(|| "未给出原因".into());
        let delay = backoff(node, attempt);
        sink.lock()
            .await
            .node(
                &node.key,
                RunEventBody::NodeRetrying {
                    attempt,
                    delay_ms: delay,
                    reason: reason.clone(),
                    feeding_error_to_model: feeds_model,
                },
            )
            .await?;
        feedback = feeds_model.then_some(reason);

        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(u64::from(delay))) => {}
            () = cancel.cancelled() => {}
        }
    }

    Ok(last)
}

/// `map` 节点：对上游数组扇出。
async fn expand_map(
    node: &NodeSpec,
    map: &ai_task_proto::MapNode,
    runner: &dyn NodeRunner,
    sink: &Mutex<dyn EventWriter>,
    ctx: &EvalContext<'_>,
    inputs: &BTreeMap<String, Value>,
    cancel: &CancellationToken,
) -> Result<NodeResult, ai_task_store::StoreError> {
    let items = match resolve(&map.over, ctx) {
        Ok(Value::Array(items)) => items,
        Ok(other) => {
            let result = NodeResult::failed(format!(
                "map 的 over 必须求值成数组，实际得到 {}",
                type_name(&other)
            ));
            emit_finished(sink, node, 0, 1, &result).await?;
            return Ok(result);
        }
        Err(err) => {
            let result = NodeResult::failed(err.to_string());
            emit_finished(sink, node, 0, 1, &result).await?;
            return Ok(result);
        }
    };

    let available = u32::try_from(items.len()).unwrap_or(u32::MAX);
    // 上限是必须的：over 来自 AI 输出，一个失控的数组能瞬间放大成几千个节点
    let taken = items.len().min(map.max_items as usize);
    if available > map.max_items {
        sink.lock()
            .await
            .node(
                &node.key,
                RunEventBody::Log {
                    level: LogLevel::Warn,
                    message: format!(
                        "上游给了 {available} 项，超过 max_items={}，只处理前 {taken} 项",
                        map.max_items
                    ),
                },
            )
            .await?;
    }

    sink.lock()
        .await
        .node(
            &node.key,
            RunEventBody::MapExpanded {
                count: u32::try_from(taken).unwrap_or(u32::MAX),
                available,
            },
        )
        .await?;

    let parallel = (map.max_parallel.max(1) as usize).min(taken.max(1));
    let mut results: Vec<Option<NodeResult>> = vec![None; taken];
    let mut cost = UsdMicros::ZERO;
    let mut cli_version = None;
    let mut first_error = None;

    // 分批并发，每批不超过 max_parallel
    for chunk_start in (0..taken).step_by(parallel) {
        if cancel.is_cancelled() {
            break;
        }
        let chunk_end = (chunk_start + parallel).min(taken);
        let mut futures = Vec::with_capacity(chunk_end - chunk_start);
        for (offset, item) in items[chunk_start..chunk_end].iter().enumerate() {
            futures.push(attempt_with_retry(
                &map.template,
                u32::try_from(chunk_start + offset).unwrap_or(u32::MAX),
                runner,
                sink,
                inputs,
                Some(item),
                cancel,
            ));
        }
        for (offset, result) in futures_util::future::join_all(futures)
            .await
            .into_iter()
            .enumerate()
        {
            let result = result?;
            cost = cost.saturating_add(result.cost);
            if result.cli_version.is_some() {
                cli_version.clone_from(&result.cli_version);
            }
            if result.status == NodeStatus::Failed && first_error.is_none() {
                first_error.clone_from(&result.error);
            }
            results[chunk_start + offset] = Some(result);
        }
    }

    // 输出是各实例结果的数组，顺序与输入一致——下游按下标就能对回去
    let collected: Vec<Value> = results
        .iter()
        .map(|r| match r {
            Some(r) if r.status == NodeStatus::Succeeded => r.output.clone().unwrap_or(Value::Null),
            Some(r) => serde_json::json!({
                "__failed": true,
                "error": r.error.clone().unwrap_or_default(),
            }),
            None => serde_json::json!({"__failed": true, "error": "未执行（run 已取消）"}),
        })
        .collect();

    let status = if cancel.is_cancelled() {
        NodeStatus::Cancelled
    } else if first_error.is_some() {
        NodeStatus::Failed
    } else {
        NodeStatus::Succeeded
    };
    let result = NodeResult {
        status,
        output: Some(Value::Array(collected)),
        error: first_error,
        cost,
        cli_version,
    };
    emit_finished(sink, node, 0, 1, &result).await?;
    Ok(result)
}

/// 解析节点声明的全部输入。
fn resolve_inputs(
    node: &NodeSpec,
    ctx: &EvalContext<'_>,
) -> Result<BTreeMap<String, Value>, String> {
    node.inputs
        .iter()
        .map(|(name, reference)| {
            resolve(reference, ctx)
                .map(|value| (name.clone(), value))
                .map_err(|err| format!("输入 `{name}` 求值失败：{err}"))
        })
        .collect()
}

/// 第 `attempt` 次失败后等多久。
fn backoff(node: &NodeSpec, attempt: u32) -> u32 {
    let factor = f64::from(node.retry.backoff_factor.max(1.0));
    let base = f64::from(node.retry.backoff_ms);
    let exponent = i32::try_from(attempt.saturating_sub(1)).unwrap_or(0);
    let delay = base * factor.powi(exponent);
    // 封顶 30 秒：指数退避不设上限的话，第 10 次重试要等几个小时
    delay.min(30_000.0) as u32
}

async fn emit_finished(
    sink: &Mutex<dyn EventWriter>,
    node: &NodeSpec,
    instance: u32,
    attempt: u32,
    result: &NodeResult,
) -> Result<(), ai_task_store::StoreError> {
    let _ = instance;
    sink.lock()
        .await
        .node(
            &node.key,
            RunEventBody::NodeFinished {
                attempt,
                status: result.status,
                output: result.output.clone(),
                error: result.error.clone(),
            },
        )
        .await
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "布尔",
        Value::Number(_) => "数字",
        Value::String(_) => "字符串",
        Value::Array(_) => "数组",
        Value::Object(_) => "对象",
    }
}

// ---------------------------------------------------------------- 调度状态

/// 一轮就绪判定的结果。
struct Ready {
    ready: Vec<NodeKey>,
    /// 入边全部否决而被跳过的节点，以及跳过的原因。
    skipped: Vec<(NodeKey, String)>,
}

/// DAG 执行过程中的节点状态。
struct RunState {
    status: BTreeMap<NodeKey, NodeStatus>,
    started: BTreeSet<NodeKey>,
}

impl RunState {
    fn new(dag: &ValidatedDag) -> Self {
        Self {
            status: dag
                .spec()
                .nodes
                .iter()
                .map(|n| (n.key.clone(), NodeStatus::Pending))
                .collect(),
            started: BTreeSet::new(),
        }
    }

    /// 取出这一轮可以执行的节点，并把它们标记为已启动。
    ///
    /// 一个节点就绪的条件：所有入边的上游都已终态，且**至少一条**入边放行。
    /// 全部否决就跳过——这是条件边的语义。
    fn take_ready(
        &mut self,
        dag: &ValidatedDag,
        outputs: &NodeOutputs,
        run_inputs: Option<&Value>,
    ) -> Ready {
        let ctx = EvalContext {
            outputs,
            run_inputs,
            map_item: None,
        };
        let mut ready = Vec::new();
        let mut skipped = Vec::new();

        for node in &dag.spec().nodes {
            if self.started.contains(&node.key) {
                continue;
            }
            let incoming: Vec<_> = dag
                .spec()
                .edges
                .iter()
                .filter(|e| e.to == node.key)
                .collect();

            if incoming.is_empty() {
                ready.push(node.key.clone());
                continue;
            }

            let upstream_all_done = incoming
                .iter()
                .all(|e| self.status.get(&e.from).is_some_and(|s| s.is_terminal()));
            if !upstream_all_done {
                continue;
            }

            let any_passes = incoming.iter().any(|edge| {
                let upstream = self
                    .status
                    .get(&edge.from)
                    .copied()
                    .unwrap_or(NodeStatus::Pending);
                eval_condition(&edge.when, upstream, &ctx).unwrap_or(false)
            });
            if any_passes {
                ready.push(node.key.clone());
            } else {
                let upstream: Vec<String> = incoming
                    .iter()
                    .map(|e| {
                        let status = self
                            .status
                            .get(&e.from)
                            .copied()
                            .unwrap_or(NodeStatus::Pending);
                        format!("{}({status:?})", e.from)
                    })
                    .collect();
                skipped.push((
                    node.key.clone(),
                    format!("入边条件全部不满足：{}", upstream.join("、")),
                ));
            }
        }

        for (key, _) in &skipped {
            self.started.insert(key.clone());
            self.status.insert(key.clone(), NodeStatus::Skipped);
        }
        for key in &ready {
            self.started.insert(key.clone());
        }
        Ready { ready, skipped }
    }

    fn finish(&mut self, key: NodeKey, status: NodeStatus) {
        self.status.insert(key, status);
    }

    /// 有节点失败且它的策略是 fail_fast 时应当停下来。
    fn should_stop(&self, dag: &ValidatedDag) -> bool {
        self.status.iter().any(|(key, status)| {
            *status == NodeStatus::Failed
                && dag
                    .node(key)
                    .is_some_and(|n| n.on_failure == ai_task_proto::OnFailure::FailFast)
                // 有失败边接出去时不算终止：那条边就是为失败准备的
                && !dag.spec().edges.iter().any(|e| {
                    &e.from == key && e.when == ai_task_proto::EdgeCondition::OnFailure
                })
        })
    }

    /// 把还没跑的节点全部标记掉。
    /// 把还没开始的节点标成跳过。
    ///
    /// `reason` 必须说清**真实原因**。预算烧穿时报"上游失败"会让人去找一个
    /// 根本不存在的失败节点——审计日志里一句说不通的话，比没有这句话更费时间。
    async fn cancel_pending(
        &mut self,
        sink: &Mutex<dyn EventWriter>,
        outcome: &mut DagOutcome,
    ) -> Result<(), ai_task_store::StoreError> {
        let pending: Vec<_> = self
            .status
            .iter()
            .filter(|(_, s)| **s == NodeStatus::Pending)
            .map(|(k, _)| k.clone())
            .collect();
        let reason = if outcome.cancelled {
            "run 已被取消"
        } else if outcome.budget_exceeded {
            "预算已用尽，未启动"
        } else {
            "上游失败，按 fail_fast 停止"
        };
        for key in pending {
            sink.lock()
                .await
                .node(
                    &key,
                    RunEventBody::NodeSkipped {
                        reason: reason.into(),
                    },
                )
                .await?;
            self.status.insert(key, NodeStatus::Skipped);
        }
        Ok(())
    }
}

/// 纯逻辑的 assert 求值（`judge` 模式要调模型，不在这里）。
pub fn eval_assert(
    assert: &AssertNode,
    inputs: &BTreeMap<String, Value>,
) -> Result<Result<(), String>, String> {
    // assert 节点约定读名为 `value` 的输入
    let value = inputs
        .get("value")
        .ok_or_else(|| "assert 节点需要一个名为 `value` 的输入".to_string())?;

    match assert {
        AssertNode::JsonSchema { schema } => Ok(match validate_output(schema, value) {
            Ok(()) => Ok(()),
            Err(errors) => Err(errors.join("；")),
        }),
        AssertNode::Expression { left, cmp, right } => {
            let outputs = NodeOutputs::new();
            let ctx = EvalContext {
                outputs: &outputs,
                run_inputs: Some(value),
                map_item: Some(value),
            };
            let actual = resolve(left, &ctx).map_err(|e| e.to_string())?;
            Ok(if ai_task_core::eval::compare(&actual, *cmp, right) {
                Ok(())
            } else {
                Err(format!(
                    "断言不成立：{} {} {}",
                    actual,
                    cmp_symbol(*cmp),
                    right
                ))
            })
        }
        AssertNode::Judge { .. } => Err("judge 模式的断言尚未实现（需要 ApiExecutor）".into()),
    }
}

fn cmp_symbol(cmp: Cmp) -> &'static str {
    match cmp {
        Cmp::Eq => "==",
        Cmp::Ne => "!=",
        Cmp::Lt => "<",
        Cmp::Lte => "<=",
        Cmp::Gt => ">",
        Cmp::Gte => ">=",
        Cmp::Contains => "contains",
        Cmp::Exists => "exists",
        Cmp::NotExists => "not exists",
    }
}

/// 让 `Arc<dyn NodeRunner>` 也能当 `&dyn NodeRunner` 用。
#[async_trait::async_trait]
impl NodeRunner for Arc<dyn NodeRunner> {
    async fn run(&self, ctx: NodeRunContext<'_>) -> NodeResult {
        self.as_ref().run(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_task_proto::{
        AiNode, DagSpec, Edge, EdgeCondition, ExecutorKind, InputRef, MapNode, OnFailure,
        RetryPolicy,
    };
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---------------------------------------------------------------- 夹具

    /// 一次节点调用里执行器看到的信息：(节点, 尝试次数, 回喂内容, map 元素)。
    type SeenCall = (String, u32, Option<String>, Option<Value>);

    /// 记录事件、不落库的写入器。
    #[derive(Default)]
    struct Recorder {
        events: Vec<(String, RunEventBody)>,
    }

    #[async_trait::async_trait]
    impl EventWriter for Recorder {
        async fn node(
            &mut self,
            node: &NodeKey,
            body: RunEventBody,
        ) -> Result<(), ai_task_store::StoreError> {
            self.events.push((node.to_string(), body));
            Ok(())
        }

        async fn run(&mut self, body: RunEventBody) -> Result<(), ai_task_store::StoreError> {
            self.events.push((String::new(), body));
            Ok(())
        }
    }

    /// 按脚本回答的节点执行器。
    struct Scripted {
        /// node key → 依次返回的结果。用完最后一个就一直返回它。
        script: BTreeMap<String, Vec<NodeResult>>,
        calls: AtomicUsize,
        /// 记录每次调用看到的信息。
        seen: std::sync::Mutex<Vec<SeenCall>>,
    }

    impl Scripted {
        fn new(script: BTreeMap<String, Vec<NodeResult>>) -> Self {
            Self {
                script,
                calls: AtomicUsize::new(0),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<SeenCall> {
            self.seen.lock().expect("锁").clone()
        }
    }

    #[async_trait::async_trait]
    impl NodeRunner for Scripted {
        async fn run(&self, ctx: NodeRunContext<'_>) -> NodeResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().expect("锁").push((
                ctx.node.key.to_string(),
                ctx.attempt,
                ctx.feedback.map(str::to_owned),
                ctx.map_item.cloned(),
            ));

            let key = ctx.node.key.to_string();
            match self.script.get(&key) {
                Some(results) if !results.is_empty() => {
                    let index = (ctx.attempt as usize - 1).min(results.len() - 1);
                    results[index].clone()
                }
                _ => NodeResult::succeeded(Some(json!({"node": key}))),
            }
        }
    }

    fn ai_node(key: &str) -> NodeSpec {
        NodeSpec {
            key: NodeKey(key.into()),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: "x".into(),
                executor: ExecutorKind::ClaudeCode,
                model: None,
                effort: None,
                skills: vec![],
                tools: vec![],
                max_turns: None,
                budget_usd: None,
            }),
            inputs: BTreeMap::new(),
            output_schema: None,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff_ms: 1,
                backoff_factor: 1.0,
                feed_error_to_model: true,
            },
            on_failure: OnFailure::FailFast,
            timeout_s: None,
            host: None,
            limits: None,
        }
    }

    fn edge(from: &str, to: &str, when: EdgeCondition) -> Edge {
        Edge {
            from: NodeKey(from.into()),
            to: NodeKey(to.into()),
            when,
        }
    }

    fn spec(nodes: Vec<NodeSpec>, edges: Vec<Edge>) -> DagSpec {
        DagSpec {
            nodes,
            edges,
            input_schema: None,
            budget_usd: None,
            timeout_s: None,
        }
    }

    async fn run(
        spec: DagSpec,
        script: BTreeMap<String, Vec<NodeResult>>,
        cancel: CancellationToken,
    ) -> (DagOutcome, Vec<(String, RunEventBody)>, Scripted) {
        let dag = ValidatedDag::validate(spec).expect("DAG 合法");
        let runner = Scripted::new(script);
        let recorder: Mutex<Recorder> = Mutex::new(Recorder::default());
        let outcome = run_dag(&dag, &runner, &recorder, None, &cancel)
            .await
            .expect("执行");
        let events = recorder.into_inner().events;
        (outcome, events, runner)
    }

    fn kinds(events: &[(String, RunEventBody)], node: &str) -> Vec<&'static str> {
        events
            .iter()
            .filter(|(n, _)| n == node)
            .map(|(_, b)| b.kind())
            .collect()
    }

    // ---------------------------------------------------------------- 测试

    /// M3 的核心验收：分析 → map 扇出 3 个 → 汇总，末节点拿到全部上游输出。
    #[tokio::test]
    async fn a_map_fanout_delivers_every_branch_result_to_the_sink() {
        let mut fan = ai_node("fix");
        fan.config = NodeConfig::Map(MapNode {
            over: InputRef::Node {
                node: NodeKey("probe".into()),
                path: "$.hosts".into(),
            },
            template: Box::new(ai_node("fix_one")),
            max_parallel: 2,
            max_items: 100,
        });

        let mut sink_node = ai_node("summary");
        sink_node.inputs.insert(
            "results".into(),
            InputRef::Node {
                node: NodeKey("fix".into()),
                path: "$".into(),
            },
        );

        let script = BTreeMap::from([
            (
                "probe".to_string(),
                vec![NodeResult::succeeded(Some(
                    json!({"hosts": ["a", "b", "c"]}),
                ))],
            ),
            (
                "fix_one".to_string(),
                vec![NodeResult::succeeded(Some(json!({"fixed": true})))],
            ),
        ]);

        let (outcome, events, runner) = run(
            spec(
                vec![ai_node("probe"), fan, sink_node],
                vec![
                    edge("probe", "fix", EdgeCondition::OnSuccess),
                    edge("fix", "summary", EdgeCondition::OnSuccess),
                ],
            ),
            script,
            CancellationToken::new(),
        )
        .await;

        assert!(outcome.failure.is_none(), "{:?}", outcome.failure);

        // map 展开事件要如实报出条数
        let expanded = events.iter().find_map(|(_, b)| match b {
            RunEventBody::MapExpanded { count, available } => Some((*count, *available)),
            _ => None,
        });
        assert_eq!(expanded, Some((3, 3)));

        // 模板节点跑了 3 次，每次拿到不同的元素
        let items: Vec<_> = runner
            .seen()
            .into_iter()
            .filter(|(k, ..)| k == "fix_one")
            .filter_map(|(.., item)| item)
            .collect();
        assert_eq!(items, vec![json!("a"), json!("b"), json!("c")]);

        // 汇总节点确实跑了，而且它看到的是全部三份结果
        assert!(
            runner.seen().iter().any(|(k, ..)| k == "summary"),
            "汇总节点应当被执行"
        );
        assert_eq!(
            outcome.outputs.get(&NodeKey("fix".into()), 0),
            Some(&json!([{"fixed": true}, {"fixed": true}, {"fixed": true}]))
        );
    }

    #[tokio::test]
    async fn a_failing_node_retries_and_the_error_is_fed_back_to_the_model() {
        let mut node = ai_node("flaky");
        node.retry.max_attempts = 3;

        let script = BTreeMap::from([(
            "flaky".to_string(),
            vec![
                NodeResult::failed("第一次挂了"),
                NodeResult::failed("第二次也挂了"),
                NodeResult::succeeded(Some(json!({"ok": true}))),
            ],
        )]);

        let (outcome, events, runner) =
            run(spec(vec![node], vec![]), script, CancellationToken::new()).await;

        assert!(outcome.failure.is_none(), "第三次应当成功");
        assert_eq!(
            kinds(&events, "flaky"),
            [
                "node_ready",
                "node_started",
                "node_retrying",
                "node_started",
                "node_retrying",
                "node_started",
                "node_finished"
            ]
        );

        // 失败原因原样回喂——模型看得见自己错在哪，才不会原样重来
        let feedback: Vec<_> = runner.seen().into_iter().map(|(.., f, _)| f).collect();
        assert_eq!(
            feedback,
            vec![None, Some("第一次挂了".into()), Some("第二次也挂了".into())]
        );
    }

    #[tokio::test]
    async fn feedback_is_withheld_when_the_policy_says_not_to_feed_it() {
        let mut node = ai_node("flaky");
        node.retry.max_attempts = 2;
        node.retry.feed_error_to_model = false;

        let script = BTreeMap::from([(
            "flaky".to_string(),
            vec![NodeResult::failed("boom"), NodeResult::succeeded(None)],
        )]);
        let (_, events, runner) =
            run(spec(vec![node], vec![]), script, CancellationToken::new()).await;

        assert!(runner.seen().iter().all(|(.., f, _)| f.is_none()));
        let retrying = events.iter().find_map(|(_, b)| match b {
            RunEventBody::NodeRetrying {
                feeding_error_to_model,
                ..
            } => Some(*feeding_error_to_model),
            _ => None,
        });
        assert_eq!(retrying, Some(false), "事件里要如实记录有没有回喂");
    }

    /// M3 验收：`output_schema` 校验不过时自动带错误重试并最终通过。
    #[tokio::test]
    async fn a_schema_violation_triggers_a_retry_with_the_validation_error() {
        let mut node = ai_node("contract");
        node.retry.max_attempts = 2;
        node.output_schema = Some(json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"]
        }));

        let script = BTreeMap::from([(
            "contract".to_string(),
            vec![
                // 第一次少了 count
                NodeResult::succeeded(Some(json!({"total": 4}))),
                NodeResult::succeeded(Some(json!({"count": 4}))),
            ],
        )]);

        let (outcome, events, runner) =
            run(spec(vec![node], vec![]), script, CancellationToken::new()).await;

        assert!(outcome.failure.is_none(), "第二次符合契约，应当成功");
        assert!(kinds(&events, "contract").contains(&"node_retrying"));

        // 回喂给模型的是**具体哪里不符**，不是一句"格式错误"
        let feedback = runner
            .seen()
            .into_iter()
            .filter_map(|(.., f, _)| f)
            .next()
            .expect("第二次应当带着校验错误");
        assert!(feedback.contains("输出不符合契约"), "{feedback}");
        assert!(feedback.contains("count"), "要说清是哪个字段：{feedback}");
    }

    #[tokio::test]
    async fn a_failure_edge_routes_around_the_failed_node() {
        let script = BTreeMap::from([("probe".to_string(), vec![NodeResult::failed("探测失败")])]);

        let (outcome, events, _) = run(
            spec(
                vec![ai_node("probe"), ai_node("ok"), ai_node("recover")],
                vec![
                    edge("probe", "ok", EdgeCondition::OnSuccess),
                    edge("probe", "recover", EdgeCondition::OnFailure),
                ],
            ),
            script,
            CancellationToken::new(),
        )
        .await;

        // 成功分支被跳过，失败分支跑了
        assert!(kinds(&events, "ok").contains(&"node_skipped"), "{events:?}");
        assert!(kinds(&events, "recover").contains(&"node_finished"));
        assert_eq!(
            outcome.outputs.get(&NodeKey("recover".into()), 0),
            Some(&json!({"node": "recover"}))
        );
    }

    #[tokio::test]
    async fn an_expression_edge_selects_a_branch_by_upstream_output() {
        let script = BTreeMap::from([(
            "probe".to_string(),
            vec![NodeResult::succeeded(Some(json!({"count": 7})))],
        )]);

        let (_, events, _) = run(
            spec(
                vec![ai_node("probe"), ai_node("many"), ai_node("few")],
                vec![
                    edge(
                        "probe",
                        "many",
                        EdgeCondition::Expr {
                            left: InputRef::Node {
                                node: NodeKey("probe".into()),
                                path: "$.count".into(),
                            },
                            cmp: Cmp::Gt,
                            right: json!(5),
                        },
                    ),
                    edge(
                        "probe",
                        "few",
                        EdgeCondition::Expr {
                            left: InputRef::Node {
                                node: NodeKey("probe".into()),
                                path: "$.count".into(),
                            },
                            cmp: Cmp::Lte,
                            right: json!(5),
                        },
                    ),
                ],
            ),
            script,
            CancellationToken::new(),
        )
        .await;

        assert!(kinds(&events, "many").contains(&"node_finished"));
        assert!(kinds(&events, "few").contains(&"node_skipped"));
    }

    #[tokio::test]
    async fn fail_fast_skips_the_rest_but_continue_keeps_going() {
        let script = BTreeMap::from([("a".to_string(), vec![NodeResult::failed("挂了")])]);

        // fail_fast：下游被跳过
        let (_, events, _) = run(
            spec(
                vec![ai_node("a"), ai_node("b")],
                vec![edge("a", "b", EdgeCondition::Always)],
            ),
            script.clone(),
            CancellationToken::new(),
        )
        .await;
        assert!(kinds(&events, "b").contains(&"node_skipped"), "{events:?}");

        // continue：下游照跑（Always 边放行）
        let mut a = ai_node("a");
        a.on_failure = OnFailure::Continue;
        let (_, events, _) = run(
            spec(
                vec![a, ai_node("b")],
                vec![edge("a", "b", EdgeCondition::Always)],
            ),
            script,
            CancellationToken::new(),
        )
        .await;
        assert!(kinds(&events, "b").contains(&"node_finished"), "{events:?}");
    }

    #[tokio::test]
    async fn map_caps_the_item_count_and_says_so() {
        let mut fan = ai_node("fan");
        fan.config = NodeConfig::Map(MapNode {
            over: InputRef::Node {
                node: NodeKey("probe".into()),
                path: "$.items".into(),
            },
            template: Box::new(ai_node("one")),
            max_parallel: 4,
            max_items: 3,
        });

        let items: Vec<Value> = (0..10).map(|i| json!(i)).collect();
        let script = BTreeMap::from([(
            "probe".to_string(),
            vec![NodeResult::succeeded(Some(json!({"items": items})))],
        )]);

        let (_, events, runner) = run(
            spec(
                vec![ai_node("probe"), fan],
                vec![edge("probe", "fan", EdgeCondition::OnSuccess)],
            ),
            script,
            CancellationToken::new(),
        )
        .await;

        // 上限必须生效：over 来自 AI 输出，失控的数组会放大成几千个节点
        let expanded = events.iter().find_map(|(_, b)| match b {
            RunEventBody::MapExpanded { count, available } => Some((*count, *available)),
            _ => None,
        });
        assert_eq!(expanded, Some((3, 10)), "要如实报出被截断了");
        assert_eq!(runner.seen().iter().filter(|(k, ..)| k == "one").count(), 3);

        // 截断这件事要在日志里说出来，不能悄悄少跑
        assert!(events.iter().any(|(_, b)| matches!(
            b,
            RunEventBody::Log { level: LogLevel::Warn, message } if message.contains("max_items")
        )));
    }

    #[tokio::test]
    async fn a_non_array_over_fails_the_map_node_with_a_readable_reason() {
        let mut fan = ai_node("fan");
        fan.config = NodeConfig::Map(MapNode {
            over: InputRef::Node {
                node: NodeKey("probe".into()),
                path: "$.count".into(),
            },
            template: Box::new(ai_node("one")),
            max_parallel: 1,
            max_items: 10,
        });
        let script = BTreeMap::from([(
            "probe".to_string(),
            vec![NodeResult::succeeded(Some(json!({"count": 4})))],
        )]);

        let (outcome, _, _) = run(
            spec(
                vec![ai_node("probe"), fan],
                vec![edge("probe", "fan", EdgeCondition::OnSuccess)],
            ),
            script,
            CancellationToken::new(),
        )
        .await;
        let failure = outcome.failure.expect("应当失败");
        assert!(failure.contains("数组"), "{failure}");
        assert!(failure.contains("数字"), "要说清实际拿到了什么：{failure}");
    }

    #[tokio::test]
    async fn a_failed_branch_does_not_lose_the_other_branches_results() {
        let mut fan = ai_node("fan");
        fan.config = NodeConfig::Map(MapNode {
            over: InputRef::Literal {
                value: json!(["a", "b"]),
            },
            template: Box::new(ai_node("one")),
            max_parallel: 1,
            max_items: 10,
        });
        // 第一个分支失败，第二个成功
        let script = BTreeMap::from([("one".to_string(), vec![NodeResult::failed("分支挂了")])]);

        let (outcome, _, _) = run(spec(vec![fan], vec![]), script, CancellationToken::new()).await;
        let output = outcome
            .outputs
            .get(&NodeKey("fan".into()), 0)
            .expect("map 节点仍要有输出");
        let array = output.as_array().expect("是数组");
        assert_eq!(array.len(), 2, "失败的分支也要占位，下游才能按下标对回去");
        assert_eq!(array[0]["__failed"], json!(true));
        assert_eq!(array[0]["error"], json!("分支挂了"));
    }

    #[tokio::test]
    async fn cancelling_stops_starting_new_nodes_and_skips_the_rest() {
        let cancel = CancellationToken::new();
        cancel.cancel();

        let (outcome, events, runner) = run(
            spec(
                vec![ai_node("a"), ai_node("b")],
                vec![edge("a", "b", EdgeCondition::OnSuccess)],
            ),
            BTreeMap::new(),
            cancel,
        )
        .await;

        assert!(outcome.cancelled);
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "取消后不该再启动节点"
        );
        for node in ["a", "b"] {
            assert!(kinds(&events, node).contains(&"node_skipped"), "{events:?}");
        }
    }

    #[tokio::test]
    async fn an_unresolvable_input_fails_the_node_with_a_readable_reason() {
        let mut node = ai_node("consumer");
        node.inputs.insert(
            "upstream".into(),
            InputRef::Node {
                node: NodeKey("probe".into()),
                path: "$.missing".into(),
            },
        );
        let script = BTreeMap::from([(
            "probe".to_string(),
            vec![NodeResult::succeeded(Some(json!({"other": 1})))],
        )]);

        let (outcome, _, _) = run(
            spec(
                vec![ai_node("probe"), node],
                vec![edge("probe", "consumer", EdgeCondition::OnSuccess)],
            ),
            script,
            CancellationToken::new(),
        )
        .await;
        let failure = outcome.failure.expect("应当失败");
        assert!(failure.contains("upstream"), "要说清是哪个输入：{failure}");
        assert!(failure.contains("$.missing"), "{failure}");
    }

    #[tokio::test]
    async fn a_node_with_all_incoming_edges_rejected_is_skipped_not_stuck() {
        // 两条入边都不放行时，节点必须被明确跳过，不能永远挂在 pending
        let script = BTreeMap::from([
            ("a".to_string(), vec![NodeResult::succeeded(None)]),
            ("b".to_string(), vec![NodeResult::succeeded(None)]),
        ]);
        let (_, events, _) = run(
            spec(
                vec![ai_node("a"), ai_node("b"), ai_node("sink")],
                vec![
                    edge("a", "sink", EdgeCondition::OnFailure),
                    edge("b", "sink", EdgeCondition::OnFailure),
                ],
            ),
            script,
            CancellationToken::new(),
        )
        .await;
        assert!(
            kinds(&events, "sink").contains(&"node_skipped"),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn a_shell_node_retry_does_not_claim_to_have_fed_a_model() {
        // shell 节点没有模型。审计日志里说"已把错误回喂给模型"就是假的。
        let mut node = ai_node("cmd");
        node.config = NodeConfig::Shell(ai_task_proto::ShellNode {
            command: "false".into(),
            working_dir: None,
        });
        node.retry.max_attempts = 2;
        node.retry.feed_error_to_model = true;

        let (_, events, runner) = run(
            spec(vec![node], vec![]),
            BTreeMap::from([("cmd".to_owned(), vec![NodeResult::failed("炸了")])]),
            CancellationToken::new(),
        )
        .await;

        let fed: Vec<bool> = events
            .iter()
            .filter_map(|(_, body)| match body {
                RunEventBody::NodeRetrying {
                    feeding_error_to_model,
                    ..
                } => Some(*feeding_error_to_model),
                _ => None,
            })
            .collect();
        assert_eq!(fed, vec![false], "shell 节点不该报回喂给模型");
        // 也真的没喂
        assert!(
            runner
                .seen()
                .iter()
                .all(|(_, _, feedback, _)| feedback.is_none()),
            "shell 节点不该收到回喂的错误"
        );
    }

    #[tokio::test]
    async fn a_run_stops_starting_nodes_once_the_budget_is_spent() {
        // 节点级的 --max-budget-usd 管不住"十个便宜节点加起来超了"。
        // run 级闸门要在批与批之间生效。
        let a = ai_node("a");
        let b = ai_node("b");
        let c = ai_node("c");
        let mut dag_spec = spec(
            vec![a, b, c],
            vec![
                edge("a", "b", EdgeCondition::OnSuccess),
                edge("b", "c", EdgeCondition::OnSuccess),
            ],
        );
        dag_spec.budget_usd = Some(UsdMicros(150_000)); // $0.15

        let costly = |cents: i64| NodeResult {
            status: NodeStatus::Succeeded,
            output: Some(json!({"ok": true})),
            error: None,
            cost: UsdMicros(cents),
            cli_version: None,
        };
        let (outcome, events, runner) = run(
            dag_spec,
            BTreeMap::from([
                ("a".to_owned(), vec![costly(100_000)]),
                ("b".to_owned(), vec![costly(100_000)]),
                ("c".to_owned(), vec![costly(100_000)]),
            ]),
            CancellationToken::new(),
        )
        .await;

        assert!(outcome.budget_exceeded, "$0.20 > $0.15 应当熔断");
        // a 和 b 跑了（超支是在 b 结束后才发现的），c 不该启动
        let ran: Vec<String> = runner.seen().into_iter().map(|(key, ..)| key).collect();
        assert_eq!(ran, ["a", "b"], "c 不该启动");
        assert_eq!(outcome.total_cost, UsdMicros(200_000));

        // 界面上要能看出是"钱花完了"而不是"任务失败了"
        let failure = outcome.failure.expect("要有原因");
        assert!(failure.contains("预算"), "{failure}");
        assert!(failure.contains("0.15"), "要报出上限：{failure}");
        assert!(failure.contains("0.20"), "要报出实际花费：{failure}");
        assert!(
            events.iter().any(|(_, body)| matches!(
                body,
                RunEventBody::Log { message, .. } if message.contains("预算上限")
            )),
            "熔断要留一条事件"
        );
    }

    #[tokio::test]
    async fn a_run_under_budget_finishes_normally() {
        let mut dag_spec = spec(
            vec![ai_node("a"), ai_node("b")],
            vec![edge("a", "b", EdgeCondition::OnSuccess)],
        );
        dag_spec.budget_usd = Some(UsdMicros(1_000_000));
        let cheap = NodeResult {
            status: NodeStatus::Succeeded,
            output: Some(json!({})),
            error: None,
            cost: UsdMicros(1_000),
            cli_version: None,
        };
        let (outcome, _, runner) = run(
            dag_spec,
            BTreeMap::from([
                ("a".to_owned(), vec![cheap.clone()]),
                ("b".to_owned(), vec![cheap]),
            ]),
            CancellationToken::new(),
        )
        .await;
        assert!(!outcome.budget_exceeded);
        assert_eq!(runner.seen().len(), 2);
    }

    #[test]
    fn backoff_grows_but_is_capped() {
        let mut node = ai_node("x");
        node.retry.backoff_ms = 1_000;
        node.retry.backoff_factor = 2.0;
        assert_eq!(backoff(&node, 1), 1_000);
        assert_eq!(backoff(&node, 2), 2_000);
        assert_eq!(backoff(&node, 3), 4_000);
        // 不封顶的话第 10 次重试要等几个小时
        assert_eq!(backoff(&node, 20), 30_000);
    }

    #[test]
    fn assert_nodes_evaluate_without_a_model() {
        let inputs = BTreeMap::from([("value".to_string(), json!({"count": 4}))]);

        let schema_ok = AssertNode::JsonSchema {
            schema: json!({"type": "object", "required": ["count"]}),
        };
        assert!(eval_assert(&schema_ok, &inputs).expect("求值").is_ok());

        let schema_bad = AssertNode::JsonSchema {
            schema: json!({"type": "object", "required": ["missing"]}),
        };
        assert!(eval_assert(&schema_bad, &inputs).expect("求值").is_err());

        let expr = AssertNode::Expression {
            left: InputRef::RunInput {
                path: "$.count".into(),
            },
            cmp: Cmp::Gt,
            right: json!(3),
        };
        assert!(eval_assert(&expr, &inputs).expect("求值").is_ok());
    }

    #[test]
    fn an_assert_without_its_input_is_a_spec_error_not_an_assertion_failure() {
        // 少了输入是编排写错了，不该显示成"断言不成立"
        let err = eval_assert(
            &AssertNode::JsonSchema { schema: json!({}) },
            &BTreeMap::new(),
        )
        .expect_err("应当报编排错误");
        assert!(err.contains("value"), "{err}");
    }
}
