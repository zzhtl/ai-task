//! 审计流水的服务端筛选与翻页。
//!
//! 这些断言的由来：筛选原本在**客户端**做——界面硬拉最近 300 条回去自己过滤。
//! 于是"最近 300 条里恰好没有"和"根本没发生过"在界面上长得一模一样，
//! 而审计恰恰是不能这样糊弄的东西。

mod common;

use ai_task_proto::WorkspaceId;
use ai_task_store::audit::{AuditEntry, AuditFilter};

async fn write(
    store: &ai_task_store::Store,
    ws: WorkspaceId,
    action: &'static str,
    kind: &'static str,
    id: &str,
    after: Option<serde_json::Value>,
) {
    store
        .audit(AuditEntry {
            workspace_id: ws,
            actor_id: None,
            action,
            target_kind: kind,
            target_id: id.to_owned(),
            before: None,
            after,
            request_id: None,
        })
        .await
        .expect("写审计");
}

const ALL: AuditFilter<'static> = AuditFilter {
    target_kind: None,
    target_id: None,
    actions: None,
    search: None,
    cursor: None,
};

db_test!(
    filtering_looks_at_the_whole_log_not_just_the_last_page,
    |f| {
        // 一条很老的记录，后面压上两百条新的。客户端过滤只看得到最近那一页，
        // 服务端过滤必须找得到它——这正是换到服务端的理由。
        write(
            &f.store,
            f.workspace,
            "host.create",
            "host",
            "针尖麦芒",
            None,
        )
        .await;
        for i in 0..200 {
            write(
                &f.store,
                f.workspace,
                "run.trigger",
                "run",
                &format!("r{i}"),
                None,
            )
            .await;
        }

        let found = f
            .store
            .list_audit(
                f.workspace,
                AuditFilter {
                    search: Some("针尖麦芒"),
                    ..ALL
                },
                50,
            )
            .await
            .expect("搜");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target_id, "针尖麦芒");
    }
);

db_test!(search_covers_the_change_payload, |f| {
    // 界面上的搜索能搜到变更内容，换到服务端不能把这个能力弄丢
    write(
        &f.store,
        f.workspace,
        "rule.update",
        "rule",
        "r1",
        Some(serde_json::json!({ "pattern": "rm -rf /" })),
    )
    .await;
    write(&f.store, f.workspace, "rule.update", "rule", "r2", None).await;

    let hit = f
        .store
        .list_audit(
            f.workspace,
            AuditFilter {
                search: Some("rm -rf"),
                ..ALL
            },
            50,
        )
        .await
        .expect("搜");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].target_id, "r1");
});

db_test!(a_percent_sign_is_a_literal_not_a_wildcard, |f| {
    // 不转义 LIKE 元字符的话，搜一个 % 就等于匹配全部——
    // 看起来像"搜什么都出来"，而不是"这个查询有问题"
    write(&f.store, f.workspace, "rule.create", "rule", "100%", None).await;
    write(&f.store, f.workspace, "rule.create", "rule", "别的", None).await;

    let hit = f
        .store
        .list_audit(
            f.workspace,
            AuditFilter {
                search: Some("%"),
                ..ALL
            },
            50,
        )
        .await
        .expect("搜");
    assert_eq!(hit.len(), 1, "% 应当被当成字面量");
    assert_eq!(hit[0].target_id, "100%");
});

db_test!(action_and_kind_filters_narrow_independently, |f| {
    write(&f.store, f.workspace, "host.create", "host", "h1", None).await;
    write(&f.store, f.workspace, "host.delete", "host", "h1", None).await;
    write(&f.store, f.workspace, "rule.create", "rule", "r1", None).await;

    let by_kind = f
        .store
        .list_audit(
            f.workspace,
            AuditFilter {
                target_kind: Some("host"),
                target_id: Some("h1"),
                ..ALL
            },
            50,
        )
        .await
        .expect("按对象");
    assert_eq!(by_kind.len(), 2);

    let actions = vec!["host.create".to_owned(), "rule.create".to_owned()];
    let by_action = f
        .store
        .list_audit(
            f.workspace,
            AuditFilter {
                actions: Some(&actions),
                ..ALL
            },
            50,
        )
        .await
        .expect("按动作");
    assert_eq!(by_action.len(), 2);
    assert!(by_action.iter().all(|r| r.action.ends_with(".create")));
});

db_test!(the_cursor_walks_the_whole_log_without_repeating, |f| {
    for i in 0..25 {
        write(
            &f.store,
            f.workspace,
            "run.trigger",
            "run",
            &format!("r{i:02}"),
            None,
        )
        .await;
    }

    let mut seen = Vec::new();
    let mut cursor = None;
    for _ in 0..10 {
        let page = f
            .store
            .list_audit(f.workspace, AuditFilter { cursor, ..ALL }, 10)
            .await
            .expect("翻页");
        if page.is_empty() {
            break;
        }
        cursor = page.last().map(|r| (r.ts, r.id));
        seen.extend(page.into_iter().map(|r| r.id));
    }

    assert_eq!(seen.len(), 25, "每条都要出现一次");
    let unique: std::collections::HashSet<_> = seen.iter().copied().collect();
    assert_eq!(unique.len(), 25, "游标不该让某条重复出现");
    // 同一毫秒内写完这 25 条完全可能，靠 id 兜住顺序
    assert!(seen.windows(2).all(|w| w[0] > w[1]), "必须严格倒序");
});

db_test!(audit_never_leaks_across_workspaces, |f| {
    write(&f.store, f.workspace, "host.create", "host", "h1", None).await;
    let other = f.new_workspace().await;
    let rows = f.store.list_audit(other, ALL, 50).await.expect("列");
    assert!(rows.is_empty());
});

db_test!(actions_and_search_are_a_union_not_an_intersection, |f| {
    // 界面上搜"触发执行"时，前端把它翻译成 action=run.trigger 一起送过来，
    // 而这四个中文字在库里任何字段上都匹配不到。两者写成 AND 的话
    // 搜中文动作名永远是空结果——上线前实测撞到过。
    write(&f.store, f.workspace, "run.trigger", "run", "r1", None).await;
    write(
        &f.store,
        f.workspace,
        "host.create",
        "host",
        "含有关键词的-h1",
        None,
    )
    .await;

    let actions = vec!["run.trigger".to_owned()];
    let both = f
        .store
        .list_audit(
            f.workspace,
            AuditFilter {
                actions: Some(&actions),
                search: Some("含有关键词的"),
                ..ALL
            },
            50,
        )
        .await
        .expect("并集");
    assert_eq!(both.len(), 2, "动作命中的和文本命中的都要出现");

    // 只给动作码时照常只按动作筛
    let only_action = f
        .store
        .list_audit(
            f.workspace,
            AuditFilter {
                actions: Some(&actions),
                ..ALL
            },
            50,
        )
        .await
        .expect("只按动作");
    assert_eq!(only_action.len(), 1);
    assert_eq!(only_action[0].action, "run.trigger");
});
