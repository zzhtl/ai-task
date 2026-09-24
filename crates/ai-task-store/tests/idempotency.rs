//! `Idempotency-Key` 的语义。
//!
//! 这套东西的文档、建表和前端请求头一直都在，**服务端一行实现都没有**——
//! 双击一次「运行」就产生两个 run，而 DTO 的文档写着这个接口需要幂等键。
//! 下面每条都对着 ADR 0002 里写死的一句话。

mod common;

use ai_task_proto::{RunEventBody, TaskId, TaskVersionId, TriggerKind, WorkspaceId};
use ai_task_store::idempotency::{IdempotentCreate, IdempotentRun};
use ai_task_store::{NewRun, PendingEvent, RunListFilter};

fn new_run(ws: WorkspaceId, task: TaskId, version: TaskVersionId, dry_run: bool) -> NewRun {
    NewRun {
        workspace_id: ws,
        task_id: task,
        task_version_id: version,
        trigger: TriggerKind::Manual,
        dry_run,
        inputs: None,
        compare_to: None,
        created_by: None,
    }
}

fn queued(version: TaskVersionId, dry_run: bool) -> PendingEvent {
    PendingEvent::run(RunEventBody::RunQueued {
        task_version_id: version,
        trigger: TriggerKind::Manual,
        inputs: None,
        dry_run,
    })
}

const ACCEPTED: i32 = 202;
const RENDER: fn(&ai_task_store::RunRecord) -> serde_json::Value =
    |run| serde_json::json!({ "id": run.id.to_string() });

db_test!(
    the_same_key_and_body_creates_one_run_and_replays_the_rest,
    |f| {
        let (task, version) = f.seed_task().await;

        let first = f
            .store
            .create_run_idempotent(
                IdempotentCreate {
                    workspace_id: f.workspace,
                    key: Some("k-1"),
                    request_hash: "hash-a",
                    status: ACCEPTED,
                },
                new_run(f.workspace, task, version, false),
                queued(version, false),
                RENDER,
            )
            .await
            .expect("第一次");
        let IdempotentRun::Created(run) = first else {
            panic!("第一次该是新建");
        };

        // 双击：同一个键、同一个 body
        let second = f
            .store
            .create_run_idempotent(
                IdempotentCreate {
                    workspace_id: f.workspace,
                    key: Some("k-1"),
                    request_hash: "hash-a",
                    status: ACCEPTED,
                },
                new_run(f.workspace, task, version, false),
                queued(version, false),
                RENDER,
            )
            .await
            .expect("第二次");
        match second {
            IdempotentRun::Replayed { status, body } => {
                assert_eq!(status, ACCEPTED);
                // 重放的必须是**第一次那个 run**，不是一个新的
                assert_eq!(body["id"], serde_json::json!(run.id.to_string()));
            }
            other => panic!(
                "第二次该是重放，实际 {other:?}",
                other = std::mem::discriminant(&other)
            ),
        }

        // 库里只有一个 run
        let runs = f
            .store
            .list_runs(f.workspace, &RunListFilter::default(), None, 100)
            .await
            .expect("列 run");
        assert_eq!(runs.len(), 1, "同一个键不该建出第二个 run");
    }
);

db_test!(the_same_key_with_a_different_body_is_a_conflict, |f| {
    let (task, version) = f.seed_task().await;
    f.store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: f.workspace,
                key: Some("k-2"),
                request_hash: "hash-a",
                status: ACCEPTED,
            },
            new_run(f.workspace, task, version, false),
            queued(version, false),
            RENDER,
        )
        .await
        .expect("第一次");

    // 同一个键，但这次是影子执行——两件不同的事，返回旧结果等于悄悄丢掉这次请求
    let clash = f
        .store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: f.workspace,
                key: Some("k-2"),
                request_hash: "hash-b",
                status: ACCEPTED,
            },
            new_run(f.workspace, task, version, true),
            queued(version, true),
            RENDER,
        )
        .await
        .expect("冲突");
    assert!(matches!(clash, IdempotentRun::Conflict));

    let runs = f
        .store
        .list_runs(f.workspace, &RunListFilter::default(), None, 100)
        .await
        .expect("列 run");
    assert_eq!(runs.len(), 1, "冲突不该建出 run");
});

db_test!(different_keys_create_different_runs, |f| {
    let (task, version) = f.seed_task().await;
    for key in ["a", "b", "c"] {
        f.store
            .create_run_idempotent(
                IdempotentCreate {
                    workspace_id: f.workspace,
                    key: Some(key),
                    request_hash: "same-hash",
                    status: ACCEPTED,
                },
                new_run(f.workspace, task, version, false),
                queued(version, false),
                RENDER,
            )
            .await
            .expect("建");
    }
    let runs = f
        .store
        .list_runs(f.workspace, &RunListFilter::default(), None, 100)
        .await
        .expect("列 run");
    assert_eq!(runs.len(), 3);
});

db_test!(no_key_means_no_deduplication, |f| {
    // 幂等键不是必需的。不给就是每次都建——不能因为 body 一样就悄悄合并，
    // "同一个任务连跑两次"是完全正常的用法。
    let (task, version) = f.seed_task().await;
    for _ in 0..2 {
        f.store
            .create_run_idempotent(
                IdempotentCreate {
                    workspace_id: f.workspace,
                    key: None,
                    request_hash: "hash-a",
                    status: ACCEPTED,
                },
                new_run(f.workspace, task, version, false),
                queued(version, false),
                RENDER,
            )
            .await
            .expect("建");
    }
    let runs = f
        .store
        .list_runs(f.workspace, &RunListFilter::default(), None, 100)
        .await
        .expect("列 run");
    assert_eq!(runs.len(), 2);
});

db_test!(an_expired_key_is_treated_as_new, |f| {
    let (task, version) = f.seed_task().await;
    f.store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: f.workspace,
                key: Some("old"),
                request_hash: "hash-a",
                status: ACCEPTED,
            },
            new_run(f.workspace, task, version, false),
            queued(version, false),
            RENDER,
        )
        .await
        .expect("第一次");

    // 把它挪到窗口之外
    sqlx::query("UPDATE idempotency_keys SET expires_at = now() - interval '1 hour'")
        .execute(f.store.pool())
        .await
        .expect("过期");

    let again = f
        .store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: f.workspace,
                key: Some("old"),
                request_hash: "hash-a",
                status: ACCEPTED,
            },
            new_run(f.workspace, task, version, false),
            queued(version, false),
            RENDER,
        )
        .await
        .expect("再来");
    assert!(
        matches!(again, IdempotentRun::Created(_)),
        "过期的键该当成没见过"
    );
});

db_test!(keys_do_not_leak_across_workspaces, |f| {
    let (task, version) = f.seed_task().await;
    f.store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: f.workspace,
                key: Some("shared"),
                request_hash: "hash-a",
                status: ACCEPTED,
            },
            new_run(f.workspace, task, version, false),
            queued(version, false),
            RENDER,
        )
        .await
        .expect("建");

    let other = f.new_workspace().await;
    // 同名的键在另一个 workspace 里必须是全新的
    let claimed = ai_task_store::idempotency::purge_expired(f.store.pool())
        .await
        .expect("清理");
    assert_eq!(claimed, 0, "没过期的不该被清掉");

    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM idempotency_keys WHERE workspace_id = $1")
            .bind(uuid::Uuid::from(other))
            .fetch_one(f.store.pool())
            .await
            .expect("数");
    assert_eq!(rows, 0);
});

db_test!(expired_keys_get_purged, |f| {
    let (task, version) = f.seed_task().await;
    f.store
        .create_run_idempotent(
            IdempotentCreate {
                workspace_id: f.workspace,
                key: Some("gone"),
                request_hash: "hash-a",
                status: ACCEPTED,
            },
            new_run(f.workspace, task, version, false),
            queued(version, false),
            RENDER,
        )
        .await
        .expect("建");
    sqlx::query("UPDATE idempotency_keys SET expires_at = now() - interval '1 day'")
        .execute(f.store.pool())
        .await
        .expect("过期");

    let purged = ai_task_store::idempotency::purge_expired(f.store.pool())
        .await
        .expect("清理");
    assert_eq!(purged, 1);
});
