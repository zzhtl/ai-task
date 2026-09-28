//! 技能的版本语义：哪一版算"最新"、什么时候插新版本、什么时候拒绝、谁在用它。

mod common;

use ai_task_proto::{SkillId, WorkspaceId};
use ai_task_store::{NewSkill, SkillUpdate, Store, StoreError};

fn skill(workspace: WorkspaceId, name: &str, version: &str, body: &str) -> NewSkill {
    NewSkill {
        workspace_id: workspace,
        name: name.into(),
        version: version.into(),
        description: "什么时候用它".into(),
        body: body.into(),
        files: vec![("ref/a.md".into(), "附件".into())],
    }
}

/// 把某一版挪到过去，免得同一毫秒里建的两版排不出先后。
async fn backdate(store: &Store, id: SkillId, minutes: i32) {
    sqlx::query("UPDATE skills SET created_at = now() - make_interval(mins => $2) WHERE id = $1")
        .bind(uuid::Uuid::from(id))
        .bind(minutes)
        .execute(store.pool())
        .await
        .expect("backdate");
}

db_test!(the_newest_version_wins_not_the_largest_string, |f| {
    // 版本号是自由文本：按文本排 "9" > "10"，以前拿到的是旧的那版
    let old = f
        .store
        .create_skill(skill(f.workspace, "triage", "9", "旧正文"))
        .await
        .expect("v9");
    backdate(&f.store, old, 10).await;
    f.store
        .create_skill(skill(f.workspace, "triage", "10", "新正文"))
        .await
        .expect("v10");

    let picked = f
        .store
        .skills_by_name(f.workspace, &["triage".into()])
        .await
        .expect("按名取");
    assert_eq!(picked[0].version, "10");
    assert_eq!(
        f.store.list_skills(f.workspace).await.expect("列表")[0].version,
        "10"
    );

    let (latest, versions) = f
        .store
        .latest_skill(f.workspace, "triage")
        .await
        .expect("最新版")
        .expect("存在");
    assert_eq!(
        (latest.version.as_str(), latest.body.as_str(), versions),
        ("10", "新正文", 2)
    );
    assert!(
        f.store
            .latest_skill(f.workspace, "nope")
            .await
            .expect("查")
            .is_none()
    );
});

db_test!(
    an_update_only_adds_a_version_when_the_content_changes,
    |f| {
        let v1 = f
            .store
            .create_skill(skill(f.workspace, "triage", "1", "正文"))
            .await
            .expect("v1");

        // 内容一样：不插新版本，哪怕带了别的版本号、或者拿着过期的 ETag（重试的场景）
        let same = f
            .store
            .update_skill(
                skill(f.workspace, "triage", "2", "正文"),
                Some(SkillId::new()),
            )
            .await
            .expect("没变");
        assert!(matches!(same, SkillUpdate::Unchanged(ref s) if s.id == v1));

        // 描述变了也算变：渐进式披露时模型只看得到它
        let mut described = skill(f.workspace, "triage", "2", "正文");
        described.description = "换个说法".into();
        let created = f
            .store
            .update_skill(described, Some(v1))
            .await
            .expect("描述变了");
        let SkillUpdate::Created { previous, current } = created else {
            panic!("描述变了应当插新版本");
        };
        assert_eq!((previous.id, current.version.as_str()), (v1, "2"));
        assert_eq!(
            previous.content_hash, current.content_hash,
            "内容指纹只管正文和附件"
        );
        let (_, versions) = f
            .store
            .latest_skill(f.workspace, "triage")
            .await
            .expect("查")
            .expect("在");
        assert_eq!(versions, 2);
    }
);

db_test!(a_stale_etag_or_a_used_version_is_refused, |f| {
    let v1 = f
        .store
        .create_skill(skill(f.workspace, "triage", "1", "正文"))
        .await
        .expect("v1");
    let SkillUpdate::Created { current: v2, .. } = f
        .store
        .update_skill(skill(f.workspace, "triage", "2", "第二版"), Some(v1))
        .await
        .expect("v2")
    else {
        panic!("应当插新版本");
    };

    // 拿着 v1 的 ETag 改：别人已经改成 v2 了
    let stale = f
        .store
        .update_skill(skill(f.workspace, "triage", "3", "我的改法"), Some(v1))
        .await;
    assert!(
        matches!(stale, Err(StoreError::Conflict { what: "skill", ref id }) if *id == v2.id.to_string()),
        "{stale:?}"
    );

    // 版本号用过了
    let reused = f
        .store
        .update_skill(skill(f.workspace, "triage", "1", "我的改法"), Some(v2.id))
        .await;
    assert!(
        matches!(
            reused,
            Err(StoreError::Conflict {
                what: "skill version",
                ..
            })
        ),
        "{reused:?}"
    );

    // 不带 ETag 就不查并发，和任务的约定一致
    assert!(matches!(
        f.store
            .update_skill(skill(f.workspace, "triage", "3", "不在乎并发"), None)
            .await,
        Ok(SkillUpdate::Created { .. })
    ));
    assert!(matches!(
        f.store
            .update_skill(skill(f.workspace, "missing", "1", "x"), None)
            .await,
        Err(StoreError::NotFound { what: "skill", .. })
    ));
});

db_test!(
    usage_is_found_anywhere_in_the_current_spec_and_only_in_this_workspace,
    |f| {
        let (_, top_version) = f.seed_task().await;
        let (_, map_version) = f.seed_task().await;
        let set_spec = |version: ai_task_proto::TaskVersionId, spec: serde_json::Value| {
            let pool = f.store.pool().clone();
            async move {
                sqlx::query("UPDATE task_versions SET dag_spec = $2 WHERE id = $1")
                    .bind(uuid::Uuid::from(version))
                    .bind(spec)
                    .execute(&pool)
                    .await
                    .expect("改 spec");
            }
        };
        set_spec(
        top_version,
        serde_json::json!({"nodes": [{"key": "a", "config": {"kind": "ai", "skills": ["triage"], "prompt": "deep"}}]}),
    )
    .await;
        set_spec(
            map_version,
            serde_json::json!({"nodes": [{"key": "m", "config": {"kind": "map", "template":
            {"key": "t", "config": {"kind": "ai", "skills": ["deep"]}}}}]}),
        )
        .await;

        let names = |skill: &'static str| {
            let store = f.store.clone();
            let ws = f.workspace;
            async move { store.tasks_using_skill(ws, skill).await.expect("查引用") }
        };
        assert_eq!(names("triage").await.len(), 1, "顶层 AI 节点");
        assert_eq!(
            names("deep").await.len(),
            1,
            "map 模板里的也算；prompt 正文里出现不算"
        );
        assert!(names("unused").await.is_empty());

        let other = f.new_workspace().await;
        assert!(
            f.store
                .tasks_using_skill(other, "triage")
                .await
                .expect("别的 workspace")
                .is_empty()
        );
    }
);

db_test!(delete_removes_every_version, |f| {
    let v1 = f
        .store
        .create_skill(skill(f.workspace, "triage", "1", "正文"))
        .await
        .expect("v1");
    f.store
        .update_skill(skill(f.workspace, "triage", "2", "第二版"), Some(v1))
        .await
        .expect("v2");
    assert_eq!(
        f.store
            .delete_skill(f.workspace, "triage")
            .await
            .expect("删"),
        2
    );
    assert!(
        f.store
            .latest_skill(f.workspace, "triage")
            .await
            .expect("查")
            .is_none()
    );
    assert_eq!(
        f.store
            .delete_skill(f.workspace, "triage")
            .await
            .expect("再删"),
        0
    );
});
