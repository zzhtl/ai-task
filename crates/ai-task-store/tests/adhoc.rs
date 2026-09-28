//! 临时批量执行的存储语义：系统任务怎么建、怎么藏，多台主机的步骤怎么算"钉在主机上"。

mod common;

use ai_task_proto::{
    DagSpec, HostId, HostSelector, NodeConfig, NodeKey, NodeSpec, ShellNode, TaskKind,
};
use ai_task_store::{NewHost, NewTask, StoreError};

fn command_spec(hosts: Vec<HostId>) -> DagSpec {
    DagSpec {
        nodes: vec![NodeSpec {
            key: NodeKey::parse("command").expect("key"),
            name: None,
            config: NodeConfig::Shell(ShellNode {
                command: "uptime".into(),
                working_dir: None,
            }),
            inputs: Default::default(),
            output_schema: None,
            retry: Default::default(),
            on_failure: Default::default(),
            timeout_s: None,
            host: Some(HostSelector::Hosts { host_ids: hosts }),
            limits: None,
        }],
        edges: vec![],
        input_schema: None,
        budget_usd: None,
        timeout_s: None,
    }
}

async fn host(f: &common::Fixture, name: &str, tags: &[&str]) -> HostId {
    let _ = ai_task_store::crypto::init(&"ab".repeat(32));
    f.store
        .create_host(NewHost {
            workspace_id: f.workspace,
            name: name.into(),
            address: "10.0.0.1".into(),
            port: 22,
            username: "root".into(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            private_key:
                "-----BEGIN OPENSSH PRIVATE KEY-----\nk\n-----END OPENSSH PRIVATE KEY-----".into(),
        })
        .await
        .expect("建主机")
}

db_test!(one_hidden_system_task_collects_every_adhoc_command, |f| {
    // 并发的第一次临时执行：系统任务只能建出一个，版本号也不能撞
    let calls = (0..5).map(|_| {
        let store = f.store.clone();
        let ws = f.workspace;
        async move {
            store
                .record_adhoc_command(ws, &command_spec(vec![HostId::new()]))
                .await
        }
    });
    let results = futures_util::future::join_all(calls).await;
    let recorded: Vec<_> = results
        .into_iter()
        .map(|r| r.expect("记下临时命令"))
        .collect();
    let task_ids: std::collections::BTreeSet<_> = recorded.iter().map(|(t, _)| *t).collect();
    assert_eq!(task_ids.len(), 1, "每个 workspace 只有一个系统任务");
    let task_id = recorded[0].0;

    let mut numbers = Vec::new();
    for (_, version_id) in &recorded {
        numbers.push(
            f.store
                .get_task_version(*version_id)
                .await
                .expect("版本")
                .version_no,
        );
    }
    numbers.sort_unstable();
    assert_eq!(numbers, [1, 2, 3, 4, 5], "每次一个新版本，版本号不重复");

    assert_eq!(
        f.store.task_kind(f.workspace, task_id).await.expect("种类"),
        Some(TaskKind::Adhoc)
    );
    // 任务列表里看不见它
    let listed = f
        .store
        .list_tasks(f.workspace, None, 100)
        .await
        .expect("列表");
    assert!(listed.iter().all(|t| t.id != task_id));
});

db_test!(the_system_task_does_not_take_a_name_from_users, |f| {
    f.store
        .record_adhoc_command(f.workspace, &command_spec(vec![HostId::new()]))
        .await
        .expect("系统任务");
    let regular = |name: &str| NewTask {
        workspace_id: f.workspace,
        name: name.into(),
        description: None,
        spec: common::minimal_spec(),
        rules: vec![],
        rules_hash: "h".into(),
        enabled: true,
    };
    // 用户照样能建一个叫「临时命令」的普通任务
    f.store
        .create_task(regular("临时命令"))
        .await
        .expect("同名的普通任务");
    // 普通任务之间还是不能重名
    let dup = f.store.create_task(regular("临时命令")).await;
    assert!(matches!(dup, Err(StoreError::Conflict { .. })), "{dup:?}");
});

db_test!(hosts_are_found_by_tag_and_pinned_by_multi_host_steps, |f| {
    let web1 = host(&f, "web-1", &["prod", "web"]).await;
    let web2 = host(&f, "web-2", &["prod", "web"]).await;
    let db = host(&f, "db-1", &["prod", "db"]).await;

    let web: Vec<String> = f
        .store
        .hosts_with_tag(f.workspace, "web")
        .await
        .expect("按 tag")
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    assert_eq!(web, ["web-1", "web-2"]);
    let briefs = f
        .store
        .host_briefs(f.workspace, &[db, web1])
        .await
        .expect("名字和 tag");
    assert_eq!(
        briefs.iter().map(|b| b.1.as_str()).collect::<Vec<_>>(),
        ["db-1", "web-1"]
    );
    assert_eq!(briefs[0].2, ["prod", "db"]);

    // 一个勾选了 web-1、web-2 的普通任务：删这两台时要被挡住
    let mut spec = command_spec(vec![web1, web2]);
    spec.nodes[0].key = NodeKey::parse("check").expect("key");
    f.store
        .create_task(NewTask {
            workspace_id: f.workspace,
            name: "巡检".into(),
            description: None,
            spec,
            rules: vec![],
            rules_hash: "h".into(),
            enabled: true,
        })
        .await
        .expect("建任务");
    assert_eq!(
        f.store
            .tasks_using_host(f.workspace, web2)
            .await
            .expect("引用"),
        ["巡检"]
    );

    // 临时命令跑过 db-1，但那只是一次性记录，不能挡住删主机
    f.store
        .record_adhoc_command(f.workspace, &command_spec(vec![db]))
        .await
        .expect("临时命令");
    assert!(
        f.store
            .tasks_using_host(f.workspace, db)
            .await
            .expect("引用")
            .is_empty()
    );
});
