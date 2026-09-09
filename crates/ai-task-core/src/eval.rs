//! 节点输入求值、边条件求值、输出契约校验。
//!
//! 全部是纯函数：不碰数据库、不碰时钟。DAG 引擎的语义都在这里定死，
//! 编排层只负责按拓扑序调用。

use std::collections::BTreeMap;

use ai_task_proto::{Cmp, EdgeCondition, InputRef, NodeKey, NodeStatus};
use serde_json::Value;
use serde_json_path::JsonPath;

/// 一次 run 里已产出的节点输出。
///
/// 键是 `(节点, 实例号)`：`map` 展开出来的实例共用一个节点 key，靠实例号区分。
#[derive(Debug, Default, Clone)]
pub struct NodeOutputs {
    values: BTreeMap<(NodeKey, u32), Value>,
}

impl NodeOutputs {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, node: NodeKey, instance: u32, output: Value) {
        self.values.insert((node, instance), output);
    }

    #[must_use]
    pub fn get(&self, node: &NodeKey, instance: u32) -> Option<&Value> {
        self.values.get(&(node.clone(), instance))
    }

    /// 取一个节点的全部实例输出，按实例号升序。
    ///
    /// 汇总节点读一个被 `map` 展开过的上游时拿到的就是这个——
    /// 它需要的是**全部**分支的结果，不是某一个。
    #[must_use]
    pub fn all_instances(&self, node: &NodeKey) -> Vec<&Value> {
        self.values
            .range((node.clone(), 0)..=(node.clone(), u32::MAX))
            .map(|(_, v)| v)
            .collect()
    }

    #[must_use]
    pub fn instance_count(&self, node: &NodeKey) -> usize {
        self.all_instances(node).len()
    }
}

/// 求值上下文。
#[derive(Debug, Clone, Copy)]
pub struct EvalContext<'a> {
    pub outputs: &'a NodeOutputs,
    /// 触发 run 时传入的参数。
    pub run_inputs: Option<&'a Value>,
    /// `map` 展开后的当前元素。只在模板节点里有值。
    pub map_item: Option<&'a Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    #[error("引用的节点 `{0}` 还没有输出")]
    MissingOutput(String),

    #[error("JSONPath `{path}` 在 `{source_desc}` 上没有匹配到任何值")]
    NoMatch { path: String, source_desc: String },

    #[error("JSONPath `{path}` 语法错误：{detail}")]
    BadPath { path: String, detail: String },

    #[error("这里不在任何 map 模板内部，取不到当前元素")]
    NoMapItem,

    #[error("触发本次 run 时没有传入参数，无法取 `{0}`")]
    NoRunInputs(String),
}

/// 把一个 [`InputRef`] 解析成具体的值。
pub fn resolve(input: &InputRef, ctx: &EvalContext<'_>) -> Result<Value, EvalError> {
    match input {
        InputRef::Literal { value } => Ok(value.clone()),
        InputRef::MapItem => ctx.map_item.cloned().ok_or(EvalError::NoMapItem),
        InputRef::RunInput { path } => {
            let source = ctx
                .run_inputs
                .ok_or_else(|| EvalError::NoRunInputs(path.clone()))?;
            query(path, source, "run 输入")
        }
        InputRef::Node { node, path } => {
            let instances = ctx.outputs.all_instances(node);
            if instances.is_empty() {
                return Err(EvalError::MissingOutput(node.to_string()));
            }
            // 上游被 map 展开过就把所有实例拼成数组再取值——汇总节点要的是
            // 全部分支的结果。只有一个实例时保持原样，避免给下游凭空套一层数组。
            let source = if instances.len() == 1 {
                instances[0].clone()
            } else {
                Value::Array(instances.into_iter().cloned().collect())
            };
            query(path, &source, &format!("节点 `{node}` 的输出"))
        }
    }
}

/// 判断一条边是否放行。
pub fn eval_condition(
    condition: &EdgeCondition,
    upstream: NodeStatus,
    ctx: &EvalContext<'_>,
) -> Result<bool, EvalError> {
    match condition {
        EdgeCondition::Always => Ok(true),
        EdgeCondition::OnSuccess => Ok(upstream == NodeStatus::Succeeded),
        // 只有真的失败才走失败边。取消和跳过不是失败——把它们也算进去，
        // 用户一按取消就会触发一堆"失败处理"分支。
        EdgeCondition::OnFailure => Ok(upstream == NodeStatus::Failed),
        EdgeCondition::Expr { left, cmp, right } => {
            // 上游没成功时不求值：它的输出要么不存在要么是半截的
            if upstream != NodeStatus::Succeeded {
                return Ok(false);
            }
            let value = resolve(left, ctx)?;
            Ok(compare(&value, *cmp, right))
        }
    }
}

/// 比较两个 JSON 值。
///
/// 数字统一按 f64 比大小（JSON 本来就没有整数/浮点之分）；
/// 类型对不上的比较一律返回 false，而不是报错——一条边求值失败就让整个 run
/// 挂掉，代价太大。
#[must_use]
pub fn compare(left: &Value, cmp: Cmp, right: &Value) -> bool {
    match cmp {
        Cmp::Eq => left == right,
        Cmp::Ne => left != right,
        Cmp::Exists => !left.is_null(),
        Cmp::NotExists => left.is_null(),
        Cmp::Contains => match (left, right) {
            (Value::String(haystack), Value::String(needle)) => haystack.contains(needle.as_str()),
            (Value::Array(items), needle) => items.contains(needle),
            (Value::Object(map), Value::String(key)) => map.contains_key(key.as_str()),
            _ => false,
        },
        Cmp::Lt | Cmp::Lte | Cmp::Gt | Cmp::Gte => {
            let (Some(a), Some(b)) = (as_number(left), as_number(right)) else {
                return false;
            };
            match cmp {
                Cmp::Lt => a < b,
                Cmp::Lte => a <= b,
                Cmp::Gt => a > b,
                Cmp::Gte => a >= b,
                _ => unreachable!("外层 match 已经限定了范围"),
            }
        }
    }
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        // 字符串形式的数字也认：模型产出的 JSON 里数字被引号包起来太常见了
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
}

/// 用 JSON Schema 校验节点输出。
///
/// 返回的错误列表会**原样回喂给模型**（见 `RetryPolicy::feed_error_to_model`），
/// 所以每条都要说清是哪个字段、错在哪。
pub fn validate_output(schema: &Value, output: &Value) -> Result<(), Vec<String>> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        // schema 本身不合法是编排的问题，不是模型的问题。照实说，
        // 别让模型对着一个坏 schema 反复重试。
        Err(err) => return Err(vec![format!("节点声明的 output_schema 本身不合法：{err}")]),
    };

    let errors: Vec<String> = validator
        .iter_errors(output)
        .map(|e| {
            let at = e.instance_path().to_string();
            let at = if at.is_empty() {
                "(根)".to_string()
            } else {
                at
            };
            format!("{at}: {e}")
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// 跑一次 JSONPath。
fn query(path: &str, source: &Value, source_desc: &str) -> Result<Value, EvalError> {
    let compiled = JsonPath::parse(path).map_err(|err| EvalError::BadPath {
        path: path.to_string(),
        detail: err.to_string(),
    })?;
    let nodes = compiled.query(source);

    match nodes.len() {
        0 => Err(EvalError::NoMatch {
            path: path.to_string(),
            source_desc: source_desc.to_string(),
        }),
        // 单个匹配就给原值，不套数组——`$.count` 应当得到 4 而不是 [4]
        1 => Ok(nodes.first().cloned().unwrap_or(Value::Null)),
        _ => Ok(Value::Array(nodes.iter().map(|v| (*v).clone()).collect())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn key(s: &str) -> NodeKey {
        NodeKey(s.into())
    }

    fn ctx<'a>(outputs: &'a NodeOutputs) -> EvalContext<'a> {
        EvalContext {
            outputs,
            run_inputs: None,
            map_item: None,
        }
    }

    #[test]
    fn a_single_match_is_returned_unwrapped() {
        let mut outputs = NodeOutputs::new();
        outputs.insert(key("probe"), 0, json!({"count": 4}));
        let value = resolve(
            &InputRef::Node {
                node: key("probe"),
                path: "$.count".into(),
            },
            &ctx(&outputs),
        )
        .expect("求值");
        // `$.count` 应当得到 4 而不是 [4]，否则下游拿到的类型全错
        assert_eq!(value, json!(4));
    }

    #[test]
    fn multiple_matches_come_back_as_an_array() {
        let mut outputs = NodeOutputs::new();
        outputs.insert(key("probe"), 0, json!({"hosts": [{"n": "a"}, {"n": "b"}]}));
        let value = resolve(
            &InputRef::Node {
                node: key("probe"),
                path: "$.hosts[*].n".into(),
            },
            &ctx(&outputs),
        )
        .expect("求值");
        assert_eq!(value, json!(["a", "b"]));
    }

    #[test]
    fn a_fanned_out_upstream_is_gathered_into_an_array_for_the_sink() {
        // 汇总节点读一个被 map 展开过的上游时，要的是**全部**分支的结果
        let mut outputs = NodeOutputs::new();
        for (i, host) in ["a", "b", "c"].iter().enumerate() {
            outputs.insert(
                key("fix"),
                u32::try_from(i).expect("小数字"),
                json!({"host": host, "ok": true}),
            );
        }
        let value = resolve(
            &InputRef::Node {
                node: key("fix"),
                path: "$[*].host".into(),
            },
            &ctx(&outputs),
        )
        .expect("求值");
        assert_eq!(value, json!(["a", "b", "c"]));
        assert_eq!(outputs.instance_count(&key("fix")), 3);
    }

    #[test]
    fn a_single_instance_is_not_wrapped_in_an_extra_array() {
        // 只有一个实例时不该凭空套一层，否则同一份 spec 在扇出 1 个和 3 个时
        // 下游要写两种取值方式
        let mut outputs = NodeOutputs::new();
        outputs.insert(key("fix"), 0, json!({"ok": true}));
        let value = resolve(
            &InputRef::Node {
                node: key("fix"),
                path: "$.ok".into(),
            },
            &ctx(&outputs),
        )
        .expect("求值");
        assert_eq!(value, json!(true));
    }

    #[test]
    fn referencing_a_node_without_output_is_an_error_not_null() {
        let outputs = NodeOutputs::new();
        assert_eq!(
            resolve(
                &InputRef::Node {
                    node: key("ghost"),
                    path: "$".into()
                },
                &ctx(&outputs)
            ),
            Err(EvalError::MissingOutput("ghost".into()))
        );
    }

    #[test]
    fn a_path_that_matches_nothing_is_an_error_not_null() {
        // 静默给 null 会让下游拿着空值继续跑，错误要到很后面才暴露
        let mut outputs = NodeOutputs::new();
        outputs.insert(key("probe"), 0, json!({"count": 4}));
        assert!(matches!(
            resolve(
                &InputRef::Node {
                    node: key("probe"),
                    path: "$.missing".into()
                },
                &ctx(&outputs)
            ),
            Err(EvalError::NoMatch { .. })
        ));
    }

    #[test]
    fn map_item_and_run_inputs_resolve_from_the_context() {
        let outputs = NodeOutputs::new();
        let item = json!({"host": "web-1"});
        let inputs = json!({"env": "staging"});
        let ctx = EvalContext {
            outputs: &outputs,
            run_inputs: Some(&inputs),
            map_item: Some(&item),
        };
        assert_eq!(resolve(&InputRef::MapItem, &ctx).expect("求值"), item);
        assert_eq!(
            resolve(
                &InputRef::RunInput {
                    path: "$.env".into()
                },
                &ctx
            )
            .expect("求值"),
            json!("staging")
        );
    }

    #[test]
    fn map_item_outside_a_template_is_an_error() {
        let outputs = NodeOutputs::new();
        assert_eq!(
            resolve(&InputRef::MapItem, &ctx(&outputs)),
            Err(EvalError::NoMapItem)
        );
    }

    #[test]
    fn failure_edges_do_not_fire_on_cancellation_or_skip() {
        // 用户一按取消就触发一堆"失败处理"分支，是很讨厌的行为
        let outputs = NodeOutputs::new();
        let c = ctx(&outputs);
        for status in [NodeStatus::Cancelled, NodeStatus::Skipped] {
            assert!(!eval_condition(&EdgeCondition::OnFailure, status, &c).expect("求值"));
        }
        assert!(eval_condition(&EdgeCondition::OnFailure, NodeStatus::Failed, &c).expect("求值"));
    }

    #[test]
    fn always_fires_regardless_of_upstream_status() {
        let outputs = NodeOutputs::new();
        let c = ctx(&outputs);
        for status in [
            NodeStatus::Succeeded,
            NodeStatus::Failed,
            NodeStatus::Cancelled,
            NodeStatus::Skipped,
        ] {
            assert!(eval_condition(&EdgeCondition::Always, status, &c).expect("求值"));
        }
    }

    #[test]
    fn an_expression_edge_does_not_evaluate_when_upstream_failed() {
        // 上游失败时它的输出要么不存在要么是半截的，求值只会产生误导
        let outputs = NodeOutputs::new();
        let cond = EdgeCondition::Expr {
            left: InputRef::Node {
                node: key("probe"),
                path: "$.count".into(),
            },
            cmp: Cmp::Gt,
            right: json!(0),
        };
        assert!(!eval_condition(&cond, NodeStatus::Failed, &ctx(&outputs)).expect("求值"));
    }

    #[test]
    fn expression_edges_compare_numbers_and_strings() {
        let mut outputs = NodeOutputs::new();
        outputs.insert(key("probe"), 0, json!({"count": 5, "level": "critical"}));
        let c = ctx(&outputs);

        let gt = EdgeCondition::Expr {
            left: InputRef::Node {
                node: key("probe"),
                path: "$.count".into(),
            },
            cmp: Cmp::Gt,
            right: json!(3),
        };
        assert!(eval_condition(&gt, NodeStatus::Succeeded, &c).expect("求值"));

        let eq = EdgeCondition::Expr {
            left: InputRef::Node {
                node: key("probe"),
                path: "$.level".into(),
            },
            cmp: Cmp::Eq,
            right: json!("critical"),
        };
        assert!(eval_condition(&eq, NodeStatus::Succeeded, &c).expect("求值"));
    }

    #[test]
    fn numeric_strings_compare_as_numbers() {
        // 模型产出的 JSON 里数字被引号包起来太常见了
        assert!(compare(&json!("5"), Cmp::Gt, &json!(3)));
        assert!(compare(&json!(5), Cmp::Gte, &json!("5")));
        // 但相等仍然是严格的：类型不同就是不同
        assert!(!compare(&json!("5"), Cmp::Eq, &json!(5)));
    }

    #[test]
    fn mismatched_types_compare_false_instead_of_erroring() {
        // 一条边求值失败就让整个 run 挂掉，代价太大
        assert!(!compare(&json!({"a": 1}), Cmp::Gt, &json!(3)));
        assert!(!compare(&json!(null), Cmp::Lt, &json!("x")));
    }

    #[test]
    fn contains_works_on_strings_arrays_and_objects() {
        assert!(compare(
            &json!("critical error"),
            Cmp::Contains,
            &json!("error")
        ));
        assert!(compare(&json!(["a", "b"]), Cmp::Contains, &json!("a")));
        assert!(compare(&json!({"cpu": 1}), Cmp::Contains, &json!("cpu")));
        assert!(!compare(&json!(["a"]), Cmp::Contains, &json!("z")));
    }

    #[test]
    fn schema_validation_reports_every_problem_with_its_location() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer"},
                "level": {"type": "string", "enum": ["ok", "warn"]}
            },
            "required": ["count", "level"]
        });
        assert!(validate_output(&schema, &json!({"count": 4, "level": "ok"})).is_ok());

        let errors = validate_output(&schema, &json!({"count": "four", "level": "boom"}))
            .expect_err("应当校验失败");
        // 错误会原样回喂给模型，所以要一次报全并说清位置
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(errors.iter().any(|e| e.contains("count")), "{errors:#?}");
        assert!(errors.iter().any(|e| e.contains("level")), "{errors:#?}");
    }

    #[test]
    fn a_broken_schema_blames_the_spec_not_the_model() {
        // 别让模型对着一个坏 schema 反复重试
        let errors = validate_output(&json!({"type": "not-a-type"}), &json!({}))
            .expect_err("坏 schema 应当被指出来");
        assert!(
            errors[0].contains("output_schema 本身不合法"),
            "{errors:#?}"
        );
    }

    #[test]
    fn root_level_errors_are_labelled_readably() {
        let errors =
            validate_output(&json!({"type": "array"}), &json!({"a": 1})).expect_err("类型不符");
        assert!(errors[0].starts_with("(根):"), "{errors:#?}");
    }
}
