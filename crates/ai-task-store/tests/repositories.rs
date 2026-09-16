//! 仓储层针对真实 PostgreSQL 的测试。
//!
//! 开关同 `schema.rs`：设 `AI_TASK_TEST_DATABASE_URL` 才跑。

mod common;

use ai_task_core::RunState;
use ai_task_proto::{
    AiNode, DagSpec, ExecutorKind, NodeConfig, NodeKey, NodeSpec, OnFailure, RetryPolicy,
    RunEventBody, RunStatus, TriggerKind, UsdMicros, WorkspaceId,
};
use ai_task_store::{
    HostUpdate, NewHost, NewRule, NewRun, NewTask, NewUser, PendingEvent, Role, RuleKind,
    RunOutcome, StoreError, UserUpdate,
};

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
        .read_events_after(run.id, 0, 100, None)
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
        .read_events_after(run.id, 0, 1_000, None)
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
        .read_events_after(run.id, 0, 1_000, None)
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
            .read_events_after(run.id, 10, 100, None)
            .await
            .expect("续传");
        assert_eq!(resumed.first().map(|e| e.seq), Some(11), "不能重发已收到的");
        assert_eq!(resumed.len(), 11);

        // 分页也不能丢事件
        let page1 = f
            .store
            .read_events_after(run.id, 0, 7, None)
            .await
            .expect("第一页");
        let page2 = f
            .store
            .read_events_after(run.id, page1.last().expect("非空").seq, 7, None)
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
        .read_events_after(run.id, 0, 10_000, None)
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
        .list_runs(f.workspace, None, None, None, 10)
        .await
        .expect("第一页");
    assert_eq!(page1.len(), 10);

    let last = page1.last().expect("非空");
    let page2 = f
        .store
        .list_runs(
            f.workspace,
            None,
            None,
            Some((last.created_at, last.id)),
            10,
        )
        .await
        .expect("第二页");
    assert_eq!(page2.len(), 10);

    let overlap: Vec<_> = page2
        .iter()
        .filter(|r| page1.iter().any(|p| p.id == r.id))
        .collect();
    assert!(overlap.is_empty(), "游标分页不能重复返回同一行");
});

db_test!(run_listing_filters_by_status, |f| {
    let queued = f.seed_run().await;
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

    let only_done = f
        .store
        .list_runs(f.workspace, None, Some(&[RunStatus::Succeeded]), None, 10)
        .await
        .expect("按状态筛");
    assert!(only_done.iter().any(|r| r.id == done.id));
    assert!(!only_done.iter().any(|r| r.id == queued.id));

    let several = f
        .store
        .list_runs(
            f.workspace,
            None,
            Some(&[RunStatus::Queued, RunStatus::Succeeded]),
            None,
            10,
        )
        .await
        .expect("多状态");
    assert!(several.iter().any(|r| r.id == queued.id));
    assert!(several.iter().any(|r| r.id == done.id));

    // 空列表是"什么都不要"，不是"不过滤"
    let none = f
        .store
        .list_runs(f.workspace, None, Some(&[]), None, 10)
        .await
        .expect("空筛选");
    assert!(none.is_empty());
});

// ---------------------------------------------------------------- 主机 / 规则 / 用户的改与删

const KEY_1: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nkey-one\n-----END OPENSSH PRIVATE KEY-----";
const KEY_2: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nkey-two\n-----END OPENSSH PRIVATE KEY-----";

fn host_update(name: &str, key: Option<&str>) -> HostUpdate {
    HostUpdate {
        name: name.into(),
        address: "10.0.0.2".into(),
        port: 2222,
        username: "deploy".into(),
        tags: vec!["staging".into()],
        private_key: key.map(str::to_owned),
    }
}

/// 私钥落库前要加密，没装 KEK 会 panic。两个用例都会调；OnceLock 只装一次，
/// 第二次返回 Err 是预期的，不是错误。
fn install_test_kek() {
    let _ = ai_task_store::crypto::init(&"ab".repeat(32));
}

async fn seed_host(f: &common::Fixture, name: &str) -> ai_task_proto::HostId {
    install_test_kek();
    f.store
        .create_host(NewHost {
            workspace_id: f.workspace,
            name: name.into(),
            address: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            tags: vec!["prod".into()],
            private_key: KEY_1.into(),
        })
        .await
        .expect("建主机")
}

db_test!(
    editing_a_host_keeps_the_key_unless_a_new_one_is_given,
    |f| {
        let id = seed_host(&f, "h1").await;

        assert!(
            f.store
                .update_host(f.workspace, id, host_update("h1-renamed", None))
                .await
                .expect("改主机")
        );
        let (host, key) = f.store.get_host(f.workspace, id).await.expect("读回");
        assert_eq!(host.name, "h1-renamed");
        assert_eq!(host.port, 2222);
        assert_eq!(host.tags, vec!["staging".to_string()]);
        assert!(key.contains("key-one"), "没给新钥匙就不能动旧钥匙");

        f.store
            .update_host(f.workspace, id, host_update("h1-renamed", Some(KEY_2)))
            .await
            .expect("换钥匙");
        let (_, key) = f.store.get_host(f.workspace, id).await.expect("读回");
        assert!(key.contains("key-two"));

        // 改完名之后，原来的名字要能再用：凭据名跟着主机名走
        seed_host(&f, "h1").await;

        // 别的 workspace 改不到、删不掉
        let other = f.new_workspace().await;
        assert!(
            !f.store
                .update_host(other, id, host_update("x", None))
                .await
                .expect("跨租户改")
        );
        assert!(!f.store.delete_host(other, id).await.expect("跨租户删"));

        assert!(f.store.delete_host(f.workspace, id).await.expect("删"));
        assert!(matches!(
            f.store.get_host(f.workspace, id).await,
            Err(StoreError::NotFound { .. })
        ));
        assert!(
            !f.store
                .delete_host(f.workspace, id)
                .await
                .expect("再删一次")
        );
    }
);

db_test!(
    tasks_pinned_to_a_host_are_listed_before_it_can_be_deleted,
    |f| {
        let id = seed_host(&f, "pinned-host").await;
        let spec: ai_task_proto::DagSpec = serde_json::from_value(serde_json::json!({
        "nodes": [{
            "key": "run",
            "config": { "kind": "shell", "command": "true" },
            "retry": { "max_attempts": 1, "backoff_ms": 1000, "backoff_factor": 2, "feed_error_to_model": false },
            "on_failure": "fail_fast",
            "host": { "on": "host", "host_id": id }
        }],
        "edges": []
    }))
    .expect("spec");
        f.store
            .create_task(NewTask {
                workspace_id: f.workspace,
                name: "pinned".into(),
                description: None,
                spec,
                rules: vec![],
                rules_hash: "h".into(),
                enabled: true,
            })
            .await
            .expect("建任务");
        // 一个不相关的任务，不该出现在结果里
        f.seed_task().await;

        assert_eq!(
            f.store
                .tasks_using_host(f.workspace, id)
                .await
                .expect("查引用"),
            vec!["pinned".to_string()]
        );
        assert!(
            f.store
                .tasks_using_host(f.workspace, ai_task_proto::HostId::new())
                .await
                .expect("无引用")
                .is_empty()
        );
    }
);

db_test!(
    rules_can_be_edited_and_deleted_and_mounting_tasks_are_found,
    |f| {
        let id = f
            .store
            .create_rule(NewRule {
                workspace_id: f.workspace,
                name: "no-restart".into(),
                spec: serde_json::json!({ "text": "不要重启" }),
                kind: RuleKind::Prompt,
                global: false,
                priority: 0,
                enabled: true,
            })
            .await
            .expect("建规则");

        assert!(
            f.store
                .update_rule(
                    f.workspace,
                    id,
                    &serde_json::json!({ "text": "先报告再重启" }),
                    true,
                    5
                )
                .await
                .expect("改规则")
        );
        let row = f
            .store
            .get_rule(f.workspace, id)
            .await
            .expect("读")
            .expect("在");
        assert_eq!(row.name, "no-restart", "名字不能被改动");
        assert_eq!(row.spec["text"], "先报告再重启");
        assert_eq!(row.scope, "global");
        assert_eq!(row.priority, 5);

        f.store
            .create_task(NewTask {
                workspace_id: f.workspace,
                name: "mounts-it".into(),
                description: None,
                spec: common::minimal_spec(),
                rules: vec!["no-restart".into()],
                rules_hash: "h".into(),
                enabled: true,
            })
            .await
            .expect("建任务");
        assert_eq!(
            f.store
                .tasks_using_rule(f.workspace, "no-restart")
                .await
                .expect("查"),
            vec!["mounts-it".to_string()]
        );
        assert!(
            f.store
                .tasks_using_rule(f.workspace, "nope")
                .await
                .expect("查")
                .is_empty()
        );

        let other = f.new_workspace().await;
        assert!(
            f.store
                .get_rule(other, id)
                .await
                .expect("跨租户读")
                .is_none()
        );
        assert!(!f.store.delete_rule(other, id).await.expect("跨租户删"));
        assert!(f.store.delete_rule(f.workspace, id).await.expect("删"));
        assert!(
            f.store
                .get_rule(f.workspace, id)
                .await
                .expect("读")
                .is_none()
        );
    }
);

db_test!(
    user_profile_and_password_can_change_and_deleting_kills_sessions,
    |f| {
        let id = f
            .store
            .create_user(NewUser {
                workspace_id: f.workspace,
                email: "a@x.io".into(),
                display_name: "A".into(),
                password: "correct horse battery".into(),
                role: Role::Operator,
            })
            .await
            .expect("建用户");

        // 只改资料，口令不动
        assert!(
            f.store
                .update_user(
                    f.workspace,
                    id,
                    UserUpdate {
                        email: "B@x.io".into(),
                        display_name: "B".into(),
                        password: None,
                    },
                )
                .await
                .expect("改资料")
        );
        assert!(
            f.store
                .login(f.workspace, "b@x.io", "correct horse battery")
                .await
                .expect("登录")
                .is_some(),
            "邮箱按小写存，旧口令仍然有效"
        );
        assert!(
            f.store
                .login(f.workspace, "a@x.io", "correct horse battery")
                .await
                .expect("旧邮箱")
                .is_none()
        );

        // 换口令
        f.store
            .update_user(
                f.workspace,
                id,
                UserUpdate {
                    email: "b@x.io".into(),
                    display_name: "B".into(),
                    password: Some("new password 12345".into()),
                },
            )
            .await
            .expect("改口令");
        assert!(
            f.store
                .login(f.workspace, "b@x.io", "correct horse battery")
                .await
                .expect("旧口令")
                .is_none()
        );
        let (token, _) = f
            .store
            .login(f.workspace, "b@x.io", "new password 12345")
            .await
            .expect("新口令")
            .expect("能登录");

        // 邮箱撞车
        f.store
            .create_user(NewUser {
                workspace_id: f.workspace,
                email: "c@x.io".into(),
                display_name: "C".into(),
                password: "another long password".into(),
                role: Role::Viewer,
            })
            .await
            .expect("第二个用户");
        assert!(matches!(
            f.store
                .update_user(
                    f.workspace,
                    id,
                    UserUpdate {
                        email: "c@x.io".into(),
                        display_name: "B".into(),
                        password: None,
                    },
                )
                .await,
            Err(StoreError::Conflict { .. })
        ));

        // 删掉：会话跟着没了
        let other = f.new_workspace().await;
        assert!(!f.store.delete_user(other, id).await.expect("跨租户删"));
        assert!(f.store.delete_user(f.workspace, id).await.expect("删"));
        assert!(
            f.store
                .principal_for(&token)
                .await
                .expect("查会话")
                .is_none()
        );
        assert!(
            !f.store
                .list_users(f.workspace)
                .await
                .expect("列表")
                .iter()
                .any(|u| u.id == id)
        );
    }
);

db_test!(the_task_cursor_walks_every_task_exactly_once, |f| {
    // `PageQuery` 一直收 cursor，但 list_tasks 以前根本不看它——
    // 客户端翻第二页会拿到和第一页一模一样的内容，而且不报错。
    for _ in 0..12 {
        f.seed_task().await;
    }

    let mut seen = Vec::new();
    let mut cursor = None;
    for _ in 0..6 {
        let page = f
            .store
            .list_tasks(f.workspace, cursor, 5)
            .await
            .expect("翻页");
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|t| (t.created_at, t.id));
        seen.extend(page.into_iter().map(|t| t.id));
    }

    assert_eq!(seen.len(), 12, "每个任务都要出现一次");
    let unique: std::collections::HashSet<_> = seen.iter().copied().collect();
    assert_eq!(unique.len(), 12, "游标不该让某个任务重复出现");
});
