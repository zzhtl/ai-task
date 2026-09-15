//! 首页聚合的窗口语义。
//!
//! 这些断言存在的理由：这个接口是用来**替掉一个算错的实现**的。
//! 之前首页拉最近 200 条 run 回浏览器里算 24 小时统计——超过 200 条之后
//! 数字就静静地偏小，而界面上看不出来。换到服务端之后，"窗口内的都算上"
//! 和"窗口外的一个都不算"这两条必须钉死，否则只是把同一个错误换了个地方。

mod common;

use ai_task_proto::WorkspaceId;

/// 把一个 run 的 created_at 挪到过去，并落到某个终态。
async fn backdate(
    store: &ai_task_store::Store,
    run: ai_task_proto::RunId,
    hours_ago: i64,
    status: &str,
    cost_micros: i64,
) {
    sqlx::query(
        "UPDATE runs SET created_at = now() - make_interval(hours => $2),
                         status = $3, cost_micros = $4, finished_at = now()
         WHERE id = $1",
    )
    .bind(uuid::Uuid::from(run))
    .bind(i32::try_from(hours_ago).expect("hours"))
    .bind(status)
    .bind(cost_micros)
    .execute(store.pool())
    .await
    .expect("backdate");
}

db_test!(window_counts_everything_inside_and_nothing_outside, |f| {
    // 三条在窗口内（2 / 10 / 23 小时前），两条在窗口外（25 / 100 小时前）
    for (hours, status, cost) in [
        (2, "succeeded", 1_000),
        (10, "failed", 2_000),
        (23, "succeeded", 3_000),
        (25, "failed", 900_000),
        (100, "succeeded", 900_000),
    ] {
        let run = f.seed_run().await;
        backdate(&f.store, run.id, hours, status, cost).await;
    }

    let stats = f
        .store
        .overview_stats(f.workspace, 24)
        .await
        .expect("stats");

    assert_eq!(stats.runs, 3, "窗口内三条");
    assert_eq!(stats.failed, 1, "窗口内只有一条失败");
    // 窗口外那两条各 0.9 USD，漏算或多算都会让这个数看起来很合理却是错的
    assert_eq!(stats.spend_usd.0, 6_000, "只把窗口内的花费加起来");
    assert_eq!(stats.window_hours, 24);
});

db_test!(a_wider_window_pulls_in_the_older_runs, |f| {
    for (hours, status, cost) in [(2, "succeeded", 1_000), (50, "failed", 5_000)] {
        let run = f.seed_run().await;
        backdate(&f.store, run.id, hours, status, cost).await;
    }

    let day = f.store.overview_stats(f.workspace, 24).await.expect("24h");
    assert_eq!(day.runs, 1);
    assert_eq!(day.spend_usd.0, 1_000);

    let week = f.store.overview_stats(f.workspace, 168).await.expect("7d");
    assert_eq!(week.runs, 2);
    assert_eq!(week.failed, 1);
    assert_eq!(week.spend_usd.0, 6_000);
});

db_test!(every_failed_terminal_status_counts_as_failed, |f| {
    // 这四个状态在界面上都是"失败"。漏掉任何一个，首页的失败数就会偏小
    for status in [
        "failed",
        "timed_out",
        "budget_exceeded",
        "resource_exceeded",
    ] {
        let run = f.seed_run().await;
        backdate(&f.store, run.id, 1, status, 0).await;
    }
    // 取消不算失败：它是人主动停的
    let cancelled = f.seed_run().await;
    backdate(&f.store, cancelled.id, 1, "cancelled", 0).await;

    let stats = f
        .store
        .overview_stats(f.workspace, 24)
        .await
        .expect("stats");
    assert_eq!(stats.runs, 5);
    assert_eq!(stats.failed, 4, "cancelled 不该算进失败");
});

db_test!(live_runs_are_counted_even_when_older_than_the_window, |f| {
    // 跑了两天还没结束的 run，"正在执行"里必须还看得见——
    // 否则一个卡住的 run 会在超过窗口之后从首页上消失，而它恰恰是最该被看见的
    let stuck = f.seed_run().await;
    backdate(&f.store, stuck.id, 48, "running", 0).await;

    let stats = f
        .store
        .overview_stats(f.workspace, 24)
        .await
        .expect("stats");
    assert_eq!(stats.running, 1, "窗口外的在跑 run 仍要计入");
    assert_eq!(stats.runs, 0, "但它不属于窗口内的统计");

    let live = f.store.overview_live(f.workspace, 20).await.expect("live");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, stuck.id);
    assert!(!live[0].task_name.is_empty(), "列表要自带任务名");
});

db_test!(overview_never_leaks_across_workspaces, |f| {
    let run = f.seed_run().await;
    backdate(&f.store, run.id, 1, "succeeded", 1_000).await;

    let other: WorkspaceId = f.new_workspace().await;
    let stats = f.store.overview_stats(other, 24).await.expect("stats");
    assert_eq!(stats.runs, 0);
    assert_eq!(stats.spend_usd.0, 0);
    assert!(
        f.store
            .overview_live(other, 20)
            .await
            .expect("live")
            .is_empty()
    );
    assert!(
        f.store
            .overview_recent(other, 10)
            .await
            .expect("recent")
            .is_empty()
    );
});
