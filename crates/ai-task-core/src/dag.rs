//! DAG 的结构校验与拓扑序。
//!
//! 校验发生在**任务入库时**，不是运行时。一个环、一个悬空的节点引用、一个语法
//! 错误的 JSONPath，都应该在保存任务时就被拒绝，而不是等到凌晨两点定时触发时
//! 才让 run 挂掉。
//!
//! 校验一次报告**全部**问题，不是遇到第一个就返回——对应 API 契约里
//! 「字段级校验错误一次返回全部」的约定。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ai_task_proto::{DagSpec, EdgeCondition, InputRef, NodeConfig, NodeKey, NodeSpec};
use serde_json_path::JsonPath;

/// 通过校验的 DAG。
///
/// 只能由 [`ValidatedDag::validate`] 构造，因此持有它就等于持有
/// 「无环、引用完整、JSONPath 语法正确」这几条不变量。
#[derive(Debug, Clone)]
pub struct ValidatedDag {
    spec: DagSpec,
    /// 拓扑序。同层内按 key 字典序，保证同一份 spec 每次算出的顺序一致
    /// ——否则事件日志的可回放性会依赖 HashMap 的迭代顺序。
    topo_order: Vec<NodeKey>,
}

impl ValidatedDag {
    /// 校验一份编排定义。
    ///
    /// 返回 `Err` 时携带**全部**问题，不是只有第一个。
    pub fn validate(spec: DagSpec) -> Result<Self, Vec<DagError>> {
        let mut errors = Vec::new();

        let nodes = index_nodes(&spec, &mut errors);
        check_edges_reference_known_nodes(&spec, &nodes, &mut errors);
        check_json_paths(&spec, &mut errors);
        check_map_item_scope(&spec, &mut errors);

        // 有悬空的边就别再谈拓扑序了，只会产出误导性的二次错误。
        let has_dangling_edge = errors
            .iter()
            .any(|e| matches!(e, DagError::UnknownEdgeEndpoint { .. }));
        let topo_order = if has_dangling_edge || nodes.is_empty() {
            Vec::new()
        } else {
            match topological_order(&spec, &nodes) {
                Ok(order) => {
                    check_inputs_reference_ancestors(&spec, &nodes, &mut errors);
                    order
                }
                Err(cycle) => {
                    errors.push(DagError::Cycle { nodes: cycle });
                    Vec::new()
                }
            }
        };

        if errors.is_empty() {
            Ok(Self { spec, topo_order })
        } else {
            Err(errors)
        }
    }

    #[must_use]
    pub fn spec(&self) -> &DagSpec {
        &self.spec
    }

    /// 拓扑序。对同一份 spec 是确定的。
    #[must_use]
    pub fn topo_order(&self) -> &[NodeKey] {
        &self.topo_order
    }

    /// 按 key 取顶层节点。不含 map 模板内部的节点。
    #[must_use]
    pub fn node(&self, key: &NodeKey) -> Option<&NodeSpec> {
        self.spec.nodes.iter().find(|n| &n.key == key)
    }

    /// 没有入边的节点，即 run 启动时立即就绪的那批。
    #[must_use]
    pub fn roots(&self) -> Vec<&NodeKey> {
        let has_incoming: BTreeSet<&NodeKey> = self.spec.edges.iter().map(|e| &e.to).collect();
        self.spec
            .nodes
            .iter()
            .map(|n| &n.key)
            .filter(|k| !has_incoming.contains(k))
            .collect()
    }
}

/// 校验失败的原因。每一条都定位到具体节点或字段。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DagError {
    #[error("DAG 至少要有一个节点")]
    Empty,

    #[error("节点 key `{key}` 重复出现 {count} 次")]
    DuplicateNodeKey { key: String, count: usize },

    #[error("边 `{from}` -> `{to}` 引用了不存在的节点 `{missing}`")]
    UnknownEdgeEndpoint {
        from: String,
        to: String,
        missing: String,
    },

    #[error("存在环：{}", nodes.join(" -> "))]
    Cycle { nodes: Vec<String> },

    #[error("节点 `{node}` 的输入 `{input}` 引用了不存在的节点 `{missing}`")]
    UnknownInputNode {
        node: String,
        input: String,
        missing: String,
    },

    #[error(
        "节点 `{node}` 的输入 `{input}` 引用了 `{referenced}`，但它不是本节点的上游——执行到本节点时它的输出还不存在"
    )]
    InputNotAnAncestor {
        node: String,
        input: String,
        referenced: String,
    },

    #[error("节点 `{node}` 的 `{field}` 不是合法的 JSONPath：{reason}")]
    InvalidJsonPath {
        node: String,
        field: String,
        reason: String,
    },

    #[error("节点 `{node}` 的输入 `{input}` 用了 map_item，但它不在任何 map 模板内部")]
    MapItemOutsideTemplate { node: String, input: String },
}

/// 建立 key -> 节点索引，顺带查重。
fn index_nodes<'a>(
    spec: &'a DagSpec,
    errors: &mut Vec<DagError>,
) -> BTreeMap<&'a NodeKey, &'a NodeSpec> {
    if spec.nodes.is_empty() {
        errors.push(DagError::Empty);
        return BTreeMap::new();
    }

    let mut counts: BTreeMap<&NodeKey, usize> = BTreeMap::new();
    for node in &spec.nodes {
        *counts.entry(&node.key).or_default() += 1;
    }
    for (key, count) in &counts {
        if *count > 1 {
            errors.push(DagError::DuplicateNodeKey {
                key: key.to_string(),
                count: *count,
            });
        }
    }

    spec.nodes.iter().map(|n| (&n.key, n)).collect()
}

fn check_edges_reference_known_nodes(
    spec: &DagSpec,
    nodes: &BTreeMap<&NodeKey, &NodeSpec>,
    errors: &mut Vec<DagError>,
) {
    for edge in &spec.edges {
        for endpoint in [&edge.from, &edge.to] {
            if !nodes.contains_key(endpoint) {
                errors.push(DagError::UnknownEdgeEndpoint {
                    from: edge.from.to_string(),
                    to: edge.to.to_string(),
                    missing: endpoint.to_string(),
                });
            }
        }
    }
}

/// 遍历所有节点（含 map 模板内部）里出现的 JSONPath，校验语法。
fn check_json_paths(spec: &DagSpec, errors: &mut Vec<DagError>) {
    for node in &spec.nodes {
        walk_nodes(node, &mut |n| {
            for (name, input) in &n.inputs {
                check_one_path(n, name, input, errors);
            }
            if let NodeConfig::Map(m) = &n.config {
                check_one_path(n, "over", &m.over, errors);
            }
            if let NodeConfig::Assert(ai_task_proto::AssertNode::Expression { left, .. }) =
                &n.config
            {
                check_one_path(n, "assert.left", left, errors);
            }
        });
    }
    for edge in &spec.edges {
        if let EdgeCondition::Expr { left, .. } = &edge.when
            && let Some(reason) = path_error(left)
        {
            errors.push(DagError::InvalidJsonPath {
                node: format!("{} -> {}", edge.from, edge.to),
                field: "when.left".into(),
                reason,
            });
        }
    }
}

fn check_one_path(node: &NodeSpec, field: &str, input: &InputRef, errors: &mut Vec<DagError>) {
    if let Some(reason) = path_error(input) {
        errors.push(DagError::InvalidJsonPath {
            node: node.key.to_string(),
            field: field.to_string(),
            reason,
        });
    }
}

fn path_error(input: &InputRef) -> Option<String> {
    let path = match input {
        InputRef::Node { path, .. } | InputRef::RunInput { path } => path,
        InputRef::MapItem | InputRef::Literal { .. } => return None,
    };
    JsonPath::parse(path).err().map(|e| e.to_string())
}

/// `map_item` 只在 map 模板内部有意义。在顶层用它，取到的永远是空。
fn check_map_item_scope(spec: &DagSpec, errors: &mut Vec<DagError>) {
    for node in &spec.nodes {
        // 顶层节点本身不在任何模板内；它的 map 模板内部则是允许的。
        for (name, input) in &node.inputs {
            if matches!(input, InputRef::MapItem) {
                errors.push(DagError::MapItemOutsideTemplate {
                    node: node.key.to_string(),
                    input: name.clone(),
                });
            }
        }
        if let NodeConfig::Map(m) = &node.config
            && matches!(m.over, InputRef::MapItem)
        {
            errors.push(DagError::MapItemOutsideTemplate {
                node: node.key.to_string(),
                input: "over".into(),
            });
        }
    }
}

/// 节点输入只能引用**真正的上游**。
///
/// 这是最容易在界面上连错、又最难在运行时诊断的一类问题：引用了一个平行分支的
/// 节点，执行到这里时那个节点可能还没跑、也可能永远不跑，拿到的是空值而不是报错。
fn check_inputs_reference_ancestors(
    spec: &DagSpec,
    nodes: &BTreeMap<&NodeKey, &NodeSpec>,
    errors: &mut Vec<DagError>,
) {
    let ancestors = compute_ancestors(spec, nodes);
    for node in &spec.nodes {
        let empty = BTreeSet::new();
        let allowed = ancestors.get(&node.key).unwrap_or(&empty);
        walk_nodes(node, &mut |n| {
            for (name, input) in &n.inputs {
                let InputRef::Node { node: target, .. } = input else {
                    continue;
                };
                if !nodes.contains_key(target) {
                    errors.push(DagError::UnknownInputNode {
                        node: n.key.to_string(),
                        input: name.clone(),
                        missing: target.to_string(),
                    });
                } else if !allowed.contains(target) {
                    errors.push(DagError::InputNotAnAncestor {
                        node: n.key.to_string(),
                        input: name.clone(),
                        referenced: target.to_string(),
                    });
                }
            }
        });
    }
}

/// 每个节点的全部传递上游。仅在已确认无环时调用。
fn compute_ancestors(
    spec: &DagSpec,
    nodes: &BTreeMap<&NodeKey, &NodeSpec>,
) -> BTreeMap<NodeKey, BTreeSet<NodeKey>> {
    let mut parents: BTreeMap<&NodeKey, Vec<&NodeKey>> = BTreeMap::new();
    for edge in &spec.edges {
        parents.entry(&edge.to).or_default().push(&edge.from);
    }

    let mut out: BTreeMap<NodeKey, BTreeSet<NodeKey>> = BTreeMap::new();
    for key in nodes.keys() {
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<&NodeKey> =
            parents.get(key).into_iter().flatten().copied().collect();
        while let Some(p) = queue.pop_front() {
            if !seen.insert(p.clone()) {
                continue;
            }
            queue.extend(parents.get(p).into_iter().flatten().copied());
        }
        out.insert((*key).clone(), seen);
    }
    out
}

/// Kahn 算法。返回 `Err` 时携带剩在环里的节点。
///
/// 手写而不是引 petgraph：这里同时需要拓扑序和上游可达集，图算法库只能省掉
/// 前者的三十行，还要多一个依赖和一次图结构转换。
fn topological_order(
    spec: &DagSpec,
    nodes: &BTreeMap<&NodeKey, &NodeSpec>,
) -> Result<Vec<NodeKey>, Vec<String>> {
    let mut in_degree: BTreeMap<&NodeKey, usize> = nodes.keys().map(|k| (*k, 0)).collect();
    let mut children: BTreeMap<&NodeKey, Vec<&NodeKey>> = BTreeMap::new();
    for edge in &spec.edges {
        children.entry(&edge.from).or_default().push(&edge.to);
        *in_degree.entry(&edge.to).or_default() += 1;
    }

    // BTreeMap 保证同层按 key 字典序出队 → 同一份 spec 的拓扑序是确定的。
    let mut ready: VecDeque<&NodeKey> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();

    let mut order = Vec::with_capacity(nodes.len());
    while let Some(key) = ready.pop_front() {
        order.push(key.clone());
        for child in children.get(key).into_iter().flatten() {
            let d = in_degree.entry(child).or_default();
            *d = d.saturating_sub(1);
            if *d == 0 {
                ready.push_back(child);
            }
        }
    }

    if order.len() == nodes.len() {
        Ok(order)
    } else {
        let mut remaining: Vec<String> = in_degree
            .iter()
            .filter(|(_, d)| **d > 0)
            .map(|(k, _)| k.to_string())
            .collect();
        remaining.sort();
        Err(remaining)
    }
}

/// 对节点及其 map 模板内部的节点递归调用 `f`。
fn walk_nodes(node: &NodeSpec, f: &mut impl FnMut(&NodeSpec)) {
    f(node);
    if let NodeConfig::Map(m) = &node.config {
        walk_nodes(&m.template, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_task_proto::{AiNode, Edge, ExecutorKind, OnFailure, RetryPolicy};

    fn key(s: &str) -> NodeKey {
        NodeKey::parse(s).expect("合法 key")
    }

    fn node(k: &str) -> NodeSpec {
        NodeSpec {
            key: key(k),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: "x".into(),
                executor: ExecutorKind::default(),
                cli: None,
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

    fn edge(from: &str, to: &str) -> Edge {
        Edge {
            from: key(from),
            to: key(to),
            when: EdgeCondition::default(),
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

    #[test]
    fn accepts_a_linear_chain_and_orders_it() {
        let dag = ValidatedDag::validate(spec(
            vec![node("c"), node("a"), node("b")],
            vec![edge("a", "b"), edge("b", "c")],
        ))
        .expect("应当通过校验");
        assert_eq!(
            dag.topo_order(),
            &[key("a"), key("b"), key("c")],
            "拓扑序必须与节点声明顺序无关"
        );
        assert_eq!(dag.roots(), vec![&key("a")]);
    }

    #[test]
    fn topological_order_is_deterministic_across_runs() {
        // 同层节点按字典序，否则事件日志的可回放性会依赖迭代顺序
        let s = spec(
            vec![node("z"), node("m"), node("a"), node("sink")],
            vec![edge("z", "sink"), edge("m", "sink"), edge("a", "sink")],
        );
        let first = ValidatedDag::validate(s.clone())
            .expect("通过")
            .topo_order()
            .to_vec();
        for _ in 0..20 {
            let again = ValidatedDag::validate(s.clone())
                .expect("通过")
                .topo_order()
                .to_vec();
            assert_eq!(first, again);
        }
        assert_eq!(first, vec![key("a"), key("m"), key("z"), key("sink")]);
    }

    #[test]
    fn rejects_empty_dag() {
        let errs = ValidatedDag::validate(spec(vec![], vec![])).expect_err("应当被拒");
        assert!(errs.contains(&DagError::Empty));
    }

    #[test]
    fn rejects_duplicate_keys() {
        let errs =
            ValidatedDag::validate(spec(vec![node("a"), node("a")], vec![])).expect_err("应当被拒");
        assert!(errs.contains(&DagError::DuplicateNodeKey {
            key: "a".into(),
            count: 2
        }));
    }

    #[test]
    fn rejects_cycles() {
        let errs = ValidatedDag::validate(spec(
            vec![node("a"), node("b"), node("c")],
            vec![edge("a", "b"), edge("b", "c"), edge("c", "a")],
        ))
        .expect_err("应当被拒");
        assert_eq!(
            errs,
            vec![DagError::Cycle {
                nodes: vec!["a".into(), "b".into(), "c".into()]
            }]
        );
    }

    #[test]
    fn rejects_self_loop() {
        let errs = ValidatedDag::validate(spec(vec![node("a")], vec![edge("a", "a")]))
            .expect_err("应当被拒");
        assert!(matches!(errs.as_slice(), [DagError::Cycle { .. }]));
    }

    #[test]
    fn rejects_dangling_edges_without_reporting_a_bogus_cycle() {
        let errs = ValidatedDag::validate(spec(vec![node("a")], vec![edge("a", "ghost")]))
            .expect_err("应当被拒");
        assert_eq!(
            errs,
            vec![DagError::UnknownEdgeEndpoint {
                from: "a".into(),
                to: "ghost".into(),
                missing: "ghost".into()
            }],
            "悬空边不应该再级联出一条误导性的环错误"
        );
    }

    #[test]
    fn reports_every_problem_at_once_not_just_the_first() {
        let errs = ValidatedDag::validate(spec(
            vec![node("a"), node("a"), node("b"), node("b")],
            vec![],
        ))
        .expect_err("应当被拒");
        assert_eq!(errs.len(), 2, "两个重复 key 都要报出来：{errs:?}");
    }

    #[test]
    fn rejects_input_referencing_a_parallel_branch() {
        // b 和 c 是 a 的两个平行分支；c 读 b 的输出，执行到 c 时 b 未必跑过
        let mut c = node("c");
        c.inputs.insert(
            "from_b".into(),
            InputRef::Node {
                node: key("b"),
                path: "$.value".into(),
            },
        );
        let errs = ValidatedDag::validate(spec(
            vec![node("a"), node("b"), c],
            vec![edge("a", "b"), edge("a", "c")],
        ))
        .expect_err("应当被拒");
        assert_eq!(
            errs,
            vec![DagError::InputNotAnAncestor {
                node: "c".into(),
                input: "from_b".into(),
                referenced: "b".into()
            }]
        );
    }

    #[test]
    fn accepts_input_referencing_a_transitive_ancestor() {
        let mut c = node("c");
        c.inputs.insert(
            "from_a".into(),
            InputRef::Node {
                node: key("a"),
                path: "$.value".into(),
            },
        );
        ValidatedDag::validate(spec(
            vec![node("a"), node("b"), c],
            vec![edge("a", "b"), edge("b", "c")],
        ))
        .expect("跨一层的上游引用应当合法");
    }

    #[test]
    fn rejects_input_referencing_an_unknown_node() {
        let mut b = node("b");
        b.inputs.insert(
            "x".into(),
            InputRef::Node {
                node: key("ghost"),
                path: "$".into(),
            },
        );
        let errs = ValidatedDag::validate(spec(vec![node("a"), b], vec![edge("a", "b")]))
            .expect_err("应当被拒");
        assert!(errs.contains(&DagError::UnknownInputNode {
            node: "b".into(),
            input: "x".into(),
            missing: "ghost".into()
        }));
    }

    #[test]
    fn rejects_malformed_jsonpath() {
        let mut b = node("b");
        b.inputs.insert(
            "bad".into(),
            InputRef::Node {
                node: key("a"),
                path: "$[".into(),
            },
        );
        let errs = ValidatedDag::validate(spec(vec![node("a"), b], vec![edge("a", "b")]))
            .expect_err("应当被拒");
        assert!(
            errs.iter().any(
                |e| matches!(e, DagError::InvalidJsonPath { node, field, .. }
                if node == "b" && field == "bad")
            ),
            "{errs:?}"
        );
    }

    #[test]
    fn rejects_map_item_at_top_level() {
        let mut a = node("a");
        a.inputs.insert("item".into(), InputRef::MapItem);
        let errs = ValidatedDag::validate(spec(vec![a], vec![])).expect_err("应当被拒");
        assert!(errs.contains(&DagError::MapItemOutsideTemplate {
            node: "a".into(),
            input: "item".into()
        }));
    }

    #[test]
    fn accepts_map_item_inside_a_template() {
        let mut inner = node("fix");
        inner.inputs.insert("item".into(), InputRef::MapItem);

        let mut fan = node("fan");
        fan.config = NodeConfig::Map(ai_task_proto::MapNode {
            over: InputRef::Node {
                node: key("probe"),
                path: "$.anomalies".into(),
            },
            template: Box::new(inner),
            max_parallel: 4,
            max_items: 100,
        });

        ValidatedDag::validate(spec(vec![node("probe"), fan], vec![edge("probe", "fan")]))
            .expect("模板内部使用 map_item 应当合法");
    }

    #[test]
    fn validates_jsonpath_inside_map_templates_too() {
        let mut inner = node("fix");
        inner.inputs.insert(
            "bad".into(),
            InputRef::RunInput {
                path: "$..[".into(),
            },
        );
        let mut fan = node("fan");
        fan.config = NodeConfig::Map(ai_task_proto::MapNode {
            over: InputRef::Node {
                node: key("probe"),
                path: "$.items".into(),
            },
            template: Box::new(inner),
            max_parallel: 1,
            max_items: 10,
        });
        let errs =
            ValidatedDag::validate(spec(vec![node("probe"), fan], vec![edge("probe", "fan")]))
                .expect_err("模板内部的错误也必须被抓到");
        assert!(
            errs.iter()
                .any(|e| matches!(e, DagError::InvalidJsonPath { node, .. } if node == "fix")),
            "{errs:?}"
        );
    }

    #[test]
    fn diamond_topology_is_valid() {
        // a -> {b, c} -> d，d 可以读 b 和 c
        let mut d = node("d");
        d.inputs.insert(
            "b".into(),
            InputRef::Node {
                node: key("b"),
                path: "$".into(),
            },
        );
        d.inputs.insert(
            "c".into(),
            InputRef::Node {
                node: key("c"),
                path: "$".into(),
            },
        );
        let dag = ValidatedDag::validate(spec(
            vec![node("a"), node("b"), node("c"), d],
            vec![
                edge("a", "b"),
                edge("a", "c"),
                edge("b", "d"),
                edge("c", "d"),
            ],
        ))
        .expect("菱形拓扑合法");
        assert_eq!(dag.topo_order().first(), Some(&key("a")));
        assert_eq!(dag.topo_order().last(), Some(&key("d")));
    }
}
