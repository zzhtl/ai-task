//! 仓储层针对真实 PostgreSQL 的测试。
//!
//! 开关同 `schema.rs`：设 `AI_TASK_TEST_DATABASE_URL` 才跑。

mod common;

use ai_task_core::RunState;
use ai_task_proto::{
    AiNode, DagSpec, ExecutorKind, NodeConfig, NodeKey, NodeSpec, OnFailure, RetryPolicy,
    RunEventBody, RunStatus, TriggerKind, UsdMicros, WorkspaceId,
};
use ai_task_store::{NewRun, NewTask, PendingEvent, RunOutcome, StoreError};

fn spec(prompt: &str) -> DagSpec {
    DagSpec {
        nodes: vec![NodeSpec {
            key: NodeKey::parse("probe").expect("key"),
            name: None,
            config: NodeConfig::Ai(AiNode {
                prompt: prompt.into(),
                executor: ExecutorKind::ClaudeCode,
                cli: None,
                model: Some("claude-haiku-4-5".into()),
                effort: None,
                skills: vec![],
                tools: vec![],
                max_turns: None,
                budget_usd: Some(UsdMicros(200_000)),
            }),
            inputs: Default::default(),
            output_schema: None,
            retry: RetryPolicy::default(),
            on_failure: OnFailure::default(),
            timeout_s: None,
            host: None,
            limits: None,
        }],
        edges: vec![],
        input_schema: None,
        budget_usd: None,
        timeout_s: None,
    }
}

fn new_task(workspace: WorkspaceId, name: &str) -> NewTask {
    NewTask {
        workspace_id: workspace,
        name: name.into(),
        description: None,
        spec: spec("统计行数"),
        rules: vec!["no-destructive".into()],
        rules_hash: "h1".into(),
        enabled: true,
    }
}

db_test!(task_and_version_round_trip_through_jsonb, |f| {
    let (task, version) = f
        .store
        .create_task(new_task(f.workspace, "probe"))
        .await
        .expect("建任务");
    assert_eq!(task.current_version_id, version.id);
    assert_eq!(version.version_no, 1);

    let fetched = f
        .store
        .get_task(f.workspace, task.id)
        .await
        .expect("取任务");
    assert_eq!(fetched.name, "probe");
    assert_eq!(fetched.version, 1, "乐观锁版本号从 1 开始");

    // DagSpec 经过 JSONB 一圈必须一模一样，否则 run 引用的快照就不可信了
    let version = f.store.get_task_version(version.id).await.expect("取版本");
    assert_eq!(version.spec, spec("统计行数"));
    assert_eq!(version.rules, vec!["no-destructive".to_string()]);
});

db_test!(a_task_is_invisible_from_another_workspace, |f| {
    let (task, _) = f
        .store
        .create_task(new_task(f.workspace, "probe"))
        .await
        .expect("建任务");

    let other = f.new_workspace().await;
    // 跨租户返回 NotFound 而不是 Forbidden：403 等于确认了这个 ID 的存在
    assert!(matches!(
        f.store.get_task(other, task.id).await,
        Err(StoreError::NotFound { .. })
    ));
});

db_test!(duplicate_task_name_is_a_conflict_not_a_raw_sqlstate, |f| {
    f.store
        .create_task(new_task(f.workspace, "dup"))
        .await
        .expect("第一次");
    let err = f
        .store
        .create_task(new_task(f.workspace, "dup"))
        .await
        .expect_err("重名必须被拒");
    assert!(
        matches!(&err, StoreError::Conflict { what, .. } if *what == "task name"),
        "错误要能直接看懂，实际：{err}"
    );
});

db_test!(creating_a_run_writes_its_first_event_atomically, |f| {
    let (task, version) = f
        .store
        .create_task(new_task(f.workspace, "probe"))
        .await
        .expect("建任务");
    let run = f
        .store
        .create_run(
            NewRun {
                workspace_id: f.workspace,
                task_id: task.id,
                task_version_id: version.id,
                trigger: TriggerKind::Manual,
                dry_run: false,
                inputs: None,
                compare_to: None,
            },
            PendingEvent::run(RunEventBody::RunQueued {
                task_version_id: version.id,
                trigger: TriggerKind::Manual,
                inputs: None,
                dry_run: false,
            }),
        )
        .await
        .expect("建 run");

    assert_eq!(run.status, RunStatus::Queued);
    assert_eq!(run.max_seq, 1, "建 run 时就该有第一条事件");

    // 不能出现「有 run 但事件流是空的」——那样回放会直接失败
    let events = f
        .store
        .read_events_after(run.id, 0, 100)
        .await
        .expect("读事件");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[0].kind(), "run_queued");
});

db_test!(batched_appends_get_contiguous_sequence_numbers, |f| {
    let run = f.seed_run().await;

    let batch: Vec<_> = (0..50)
        .map(|i| {
            PendingEvent::new(
                Some("probe".into()),
                RunEventBody::AgentText {
                    text: format!("chunk {i}"),
                },
            )
        })
        .collect();
    let last = f.store.append_events(run.id, &batch).await.expect("批量写");
    assert_eq!(last, 51, "首条 run_queued 占了 seq=1");

    let events = f
        .store
        .read_events_after(run.id, 0, 1_000)
        .await
        .expect("读事件");
    let seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=51).collect::<Vec<_>>(), "seq 必须连续无洞");
});

db_test!(empty_batch_is_a_no_op, |f| {
    let run = f.seed_run().await;
    let seq = f.store.append_events(run.id, &[]).await.expect("空批次");
    assert_eq!(seq, 1, "定时刷新没攒到东西时不该推进 seq");
});

db_test!(concurrent_writers_never_collide_on_seq, |f| {
    // seq 由 `UPDATE ... RETURNING` 在行锁下分配，所以即使引擎重启时新旧写入
    // 任务短暂并存，也不会撞号。这条测试就是在验这个，而不是靠"单写者"的约定。
    let run = f.seed_run().await;
    let mut tasks = Vec::new();
    for worker in 0..8 {
        let store = f.store.clone();
        let run_id = run.id;
        tasks.push(tokio::spawn(async move {
            for i in 0..10 {
                store
                    .append_events(
                        run_id,
                        &[PendingEvent::new(
                            Some("probe".into()),
                            RunEventBody::AgentText {
                                text: format!("w{worker}-{i}"),
                            },
                        )],
                    )
                    .await
                    .expect("并发写");
            }
        }));
    }
    for task in tasks {
        task.await.expect("worker");
    }

    let events = f
        .store
        .read_events_after(run.id, 0, 1_000)
        .await
        .expect("读事件");
    let mut seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), 81, "8×10 条 + 首条，不能有重复或丢失");
    assert_eq!(seqs, (1..=81).collect::<Vec<_>>());
});

db_test!(
    reading_after_a_sequence_number_supports_sse_resumption,
    |f| {
        let run = f.seed_run().await;
        let batch: Vec<_> = (0..20)
            .map(|i| {
                PendingEvent::new(
                    Some("probe".into()),
                    RunEventBody::AgentText {
                        text: i.to_string(),
                    },
                )
            })
            .collect();
        f.store.append_events(run.id, &batch).await.expect("写事件");

        // 客户端断在 seq=10，重连时带 Last-Event-ID: 10
        let resumed = f
            .store
            .read_events_after(run.id, 10, 100)
            .await
            .expect("续传");
        assert_eq!(resumed.first().map(|e| e.seq), Some(11), "不能重发已收到的");
        assert_eq!(resumed.len(), 11);

        // 分页也不能丢事件
        let page1 = f
            .store
            .read_events_after(run.id, 0, 7)
            .await
            .expect("第一页");
        let page2 = f
            .store
            .read_events_after(run.id, page1.last().expect("非空").seq, 7)
            .await
            .expect("第二页");
        assert_eq!(page1.len(), 7);
        assert_eq!(page2.first().map(|e| e.seq), Some(8));
    }
);

db_test!(a_finished_run_replays_from_its_event_log_alone, |f| {
    // M1 的核心验收：只用 run_events 就能重建出终态
    let run = f.seed_run().await;
    f.store
        .start_run(
            run.id,
            Some("2.1.263"),
            Some("fp-test"),
            PendingEvent::run(RunEventBody::RunStarted {
                worker: "test".into(),
            }),
        )
        .await
        .expect("开始");
    f.store
        .append_events(
            run.id,
            &[
                PendingEvent::new(Some("probe".into()), RunEventBody::NodeReady),
                PendingEvent::new(
                    Some("probe".into()),
                    RunEventBody::NodeStarted {
                        attempt: 1,
                        host_id: None,
                    },
                ),
                PendingEvent::new(
                    Some("probe".into()),
                    RunEventBody::NodeFinished {
                        attempt: 1,
                        status: ai_task_proto::NodeStatus::Succeeded,
                        output: Some(serde_json::json!({"lines": 4})),
                        error: None,
                    },
                ),
            ],
        )
        .await
        .expect("节点事件");
    f.store
        .finish_run(
            run.id,
            RunOutcome {
                status: RunStatus::Succeeded,
                cost: UsdMicros(23_259),
                output: Some(serde_json::json!({"lines": 4})),
                error: None,
                cli_version: None,
                output_digest: None,
            },
            PendingEvent::run(RunEventBody::RunFinished {
                status: RunStatus::Succeeded,
                error: None,
                cost_usd: UsdMicros(23_259),
            }),
        )
        .await
        .expect("结束");

    let events = f
        .store
        .read_events_after(run.id, 0, 10_000)
        .await
        .expect("读事件");
    let state = RunState::replay(&events).expect("重放");

    assert_eq!(state.status, RunStatus::Succeeded);
    assert_eq!(state.cost, UsdMicros(23_259));
    assert_eq!(
        state.nodes[&NodeKey("probe".into())].status,
        ai_task_proto::NodeStatus::Succeeded
    );
    assert!(state.unfinished_nodes().is_empty());

    // 投影列必须和重放结果一致，否则列表页和详情页会互相打架
    let stored = f.store.get_run(f.workspace, run.id).await.expect("取 run");
    assert_eq!(stored.status, state.status);
    assert_eq!(stored.cost, state.cost);
    assert_eq!(stored.max_seq, state.last_seq);
    assert_eq!(stored.cli_version.as_deref(), Some("2.1.263"));
});

db_test!(
    unfinished_runs_are_what_the_engine_recovers_on_restart,
    |f| {
        let running = f.seed_run().await;
        f.store
            .start_run(
                running.id,
                None,
                None,
                PendingEvent::run(RunEventBody::RunStarted { worker: "w".into() }),
            )
            .await
            .expect("开始");

        let done = f.seed_run().await;
        f.store
            .finish_run(
                done.id,
                RunOutcome {
                    status: RunStatus::Succeeded,
                    cost: UsdMicros::ZERO,
                    output: None,
                    error: None,
                    cli_version: None,
                    output_digest: None,
                },
                PendingEvent::run(RunEventBody::RunFinished {
                    status: RunStatus::Succeeded,
                    error: None,
                    cost_usd: UsdMicros::ZERO,
                }),
            )
            .await
            .expect("结束");

        let pending = f.store.unfinished_runs(100).await.expect("扫描");
        let ids: Vec<_> = pending.iter().map(|r| r.id).collect();
        assert!(ids.contains(&running.id));
        assert!(!ids.contains(&done.id), "已完成的 run 不该被重新拉起");
    }
);

db_test!(run_listing_uses_a_keyset_cursor, |f| {
    for _ in 0..25 {
        f.seed_run().await;
    }
    let page1 = f
        .store
        .list_runs(f.workspace, None, None, 10)
        .await
        .expect("第一页");
    assert_eq!(page1.len(), 10);

    let last = page1.last().expect("非空");
    let page2 = f
        .store
        .list_runs(f.workspace, None, Some((last.created_at, last.id)), 10)
        .await
        .expect("第二页");
    assert_eq!(page2.len(), 10);

    let overlap: Vec<_> = page2
        .iter()
        .filter(|r| page1.iter().any(|p| p.id == r.id))
        .collect();
    assert!(overlap.is_empty(), "游标分页不能重复返回同一行");
});
