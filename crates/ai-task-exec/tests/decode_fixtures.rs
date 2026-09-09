//! 用真实录制的 `stream-json` 回放整条解码链路。
//!
//! CLI 的事件形状不是稳定契约。升级 claude 之后重新录一份样本（录制命令见
//! `fixtures/README.md`）跑这组测试，就知道防腐层要不要跟着改——而不是等某天
//! 凌晨的定时任务挂了才发现。
//!
//! 期望值不是从实现里抄的，是先用 jq 从样本里算出来再写进断言的。

use ai_task_exec::claude_code::decode::Decoder;
use ai_task_exec::{ExecEvent, ExecOutcome, ToolOutcome};
use ai_task_proto::UsdMicros;

const HERMETIC: &str = include_str!("fixtures/hermetic-count-lines.jsonl");
const INHERITED: &str = include_str!("fixtures/inherited-mcp-count-lines.jsonl");
const BUDGET_EXCEEDED: &str = include_str!("fixtures/budget-exceeded.jsonl");

fn replay(jsonl: &str) -> Vec<ExecEvent> {
    let mut decoder = Decoder::new();
    jsonl.lines().flat_map(|line| decoder.line(line)).collect()
}

fn count(events: &[ExecEvent], predicate: impl Fn(&ExecEvent) -> bool) -> usize {
    events.iter().filter(|e| predicate(e)).count()
}

#[test]
fn hermetic_run_decodes_to_the_expected_event_sequence() {
    let events = replay(HERMETIC);

    let Some(ExecEvent::Started {
        tools,
        cli_version,
        model,
        session_id,
    }) = events.first()
    else {
        panic!("第一条必须是 Started：{:?}", events.first());
    };
    assert_eq!(tools.len(), 3, "密闭执行下只应有请求的 3 个工具：{tools:?}");
    assert_eq!(cli_version.as_deref(), Some("2.1.263"));
    assert_eq!(model, "claude-haiku-4-5");
    assert!(!session_id.is_empty());

    // 样本里有 5 个 thinking 块，但**每一个的 thinking 字段都是空串**——
    // 非 summarized 模式下 CLI 只给签名。所以一条 Thinking 事件都不该产生：
    // 空事件写进 append-only 日志只是每个 run 多几行噪音。
    //
    // 两个数一起断言，是为了在 CLI 哪天真的开始给内容时能看出区别：
    // 那时块数不变而事件数会跟着变，这组测试就该红。
    assert_eq!(
        HERMETIC.matches(r#""type":"thinking""#).count(),
        5,
        "样本里的 thinking 块数变了，重新录过？"
    );
    assert_eq!(
        count(&events, |e| matches!(e, ExecEvent::Thinking { .. })),
        0,
        "样本里的 thinking 块全是签名，不该产生事件"
    );
    assert_eq!(count(&events, |e| matches!(e, ExecEvent::Text { .. })), 1);
    assert_eq!(
        count(&events, |e| matches!(e, ExecEvent::ToolRequested { .. })),
        4
    );
    assert_eq!(
        count(&events, |e| matches!(e, ExecEvent::ToolCompleted { .. })),
        4
    );
    assert_eq!(
        count(&events, |e| matches!(e, ExecEvent::PermissionDenied { .. })),
        2
    );

    // 权威用量只有一条，来自 result
    let usage: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ExecEvent::Usage {
                cost_usd,
                output_tokens,
                input_tokens,
                ..
            } => Some((*cost_usd, *output_tokens, *input_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(usage.len(), 1, "一次执行只该有一条权威用量");
    assert_eq!(usage[0], (UsdMicros(33_570), 713, 959));

    assert_eq!(
        events.last(),
        Some(&ExecEvent::Finished(ExecOutcome::Success {
            result: "4".into(),
            turns: 5
        }))
    );
}

/// 逐条累加 `assistant` 事件的 usage 会得到一个和账单差两个数量级的数字。
/// 这条测试把「只信 result」这个决策钉住。
#[test]
fn per_message_usage_is_never_emitted_as_cost() {
    let events = replay(HERMETIC);
    let usage_events = count(&events, |e| matches!(e, ExecEvent::Usage { .. }));
    assert_eq!(
        usage_events, 1,
        "样本里有 10 条 assistant 事件各自带着 usage；照单发出去会让预算熔断形同虚设"
    );

    // 样本实测：照单累加 output_tokens = 16，按 message.id 去重 = 8，
    // 而 result 的权威值是 713。
    let ExecEvent::Usage { output_tokens, .. } = events
        .iter()
        .find(|e| matches!(e, ExecEvent::Usage { .. }))
        .expect("应当有一条用量")
    else {
        unreachable!()
    };
    assert_eq!(*output_tokens, 713);
    assert!(*output_tokens > 100, "取到的显然不是那个 8 或 16");
}

#[test]
fn budget_exhaustion_is_a_distinct_outcome_not_a_generic_failure() {
    let events = replay(BUDGET_EXCEEDED);
    assert_eq!(
        events.last(),
        Some(&ExecEvent::Finished(ExecOutcome::BudgetExceeded)),
        "熔断要能和普通失败区分开，否则 run 的终态说不清"
    );

    // 预算耗尽时 CLI 的 `usage` 整个是 0，只有 modelUsage 还是对的。
    // 取错字段的话，被熔断的 run 会显示成本为零。
    let ExecEvent::Usage {
        cost_usd,
        output_tokens,
        ..
    } = events
        .iter()
        .find(|e| matches!(e, ExecEvent::Usage { .. }))
        .expect("熔断也必须报出已花掉的钱")
    else {
        unreachable!()
    };
    assert_eq!(*cost_usd, UsdMicros(5_519));
    assert_eq!(*output_tokens, 355);
}

/// 不做隔离时 CLI 会把宿主的 MCP server 全拉进来。这条测试不是要它通过，
/// 是把「不隔离会发生什么」记录在案：工具从 3 个涨到 30 个，成本翻一倍。
#[test]
fn inheriting_host_config_inflates_the_tool_surface_and_the_bill() {
    let hermetic = replay(HERMETIC);
    let inherited = replay(INHERITED);

    let tools_of = |events: &[ExecEvent]| match events.first() {
        Some(ExecEvent::Started { tools, .. }) => tools.len(),
        other => panic!("第一条必须是 Started：{other:?}"),
    };
    let cost_of = |events: &[ExecEvent]| {
        events
            .iter()
            .find_map(|e| match e {
                ExecEvent::Usage { cost_usd, .. } => Some(*cost_usd),
                _ => None,
            })
            .expect("应当有用量")
    };

    assert_eq!(tools_of(&hermetic), 3);
    assert_eq!(tools_of(&inherited), 30);
    assert_eq!(cost_of(&hermetic), UsdMicros(33_570));
    assert_eq!(cost_of(&inherited), UsdMicros(73_607));
    assert!(
        cost_of(&inherited).0 > cost_of(&hermetic).0 * 2,
        "同一个任务，继承宿主配置贵了一倍以上"
    );
}

/// 样本里出现 Warning，说明 CLI 吐了防腐层不认识的东西——升级后第一个要看的信号。
#[test]
fn current_cli_output_is_fully_understood() {
    for (name, jsonl) in [
        ("hermetic", HERMETIC),
        ("inherited", INHERITED),
        ("budget", BUDGET_EXCEEDED),
    ] {
        let warnings: Vec<_> = replay(jsonl)
            .into_iter()
            .filter_map(|e| match e {
                ExecEvent::Warning { message } => Some(message),
                _ => None,
            })
            .collect();
        assert!(
            warnings.is_empty(),
            "{name} 样本里出现了看不懂的事件，防腐层需要更新：{warnings:#?}"
        );
    }
}

#[test]
fn denied_tool_calls_surface_both_the_denial_and_the_error_result() {
    let events = replay(HERMETIC);

    // 一次被拒的调用会同时产生 system/permission_denied 和一条 is_error 的
    // tool_result。两条都要留痕：前者带拒绝理由，后者是模型实际看到的东西。
    let denied_ids: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ExecEvent::PermissionDenied {
                tool_use_id,
                reason,
                ..
            } => {
                assert!(!reason.is_empty(), "拒绝必须带原因");
                Some(tool_use_id.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(denied_ids.len(), 2);

    for id in denied_ids {
        let matched = events.iter().any(|e| {
            matches!(e, ExecEvent::ToolCompleted { tool_use_id, outcome, .. }
                if *tool_use_id == id && *outcome == ToolOutcome::Error)
        });
        assert!(matched, "被拒的调用 {id} 没有对应的错误 tool_result");
    }
}

#[test]
fn every_tool_request_is_paired_with_a_completion() {
    let events = replay(HERMETIC);
    let requested: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ExecEvent::ToolRequested { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect();
    let completed: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ExecEvent::ToolCompleted { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(requested.len(), 4);
    for id in &requested {
        assert!(
            completed.contains(id),
            "工具调用 {id} 没有结果，界面上会一直转圈"
        );
    }
}

#[test]
fn a_terminal_event_is_always_last_and_only_once() {
    for jsonl in [HERMETIC, INHERITED, BUDGET_EXCEEDED] {
        let events = replay(jsonl);
        assert_eq!(
            count(&events, ExecEvent::is_terminal),
            1,
            "终态事件必须恰好一条，否则 run 会看到两个结局"
        );
        assert!(events.last().is_some_and(ExecEvent::is_terminal));
    }
}
