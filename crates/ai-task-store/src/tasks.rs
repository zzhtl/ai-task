//! 任务与任务版本。
//!
//! 每次更新任务定义都产生一个**不可变版本**，run 引用的是版本而不是任务。
//! 没有这层，改完定义后历史 run 就无法解释、无法回放，漂移检测也失去基线。

use ai_task_proto::{DagSpec, TaskId, TaskVersionId, WorkspaceId};
use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Store, StoreError};

/// 任务的当前状态。
#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub id: TaskId,
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub description: Option<String>,
    pub current_version_id: TaskVersionId,
    pub enabled: bool,
    /// 乐观锁版本号，同时是 `ETag` 的取值。
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 一个不可变的定义快照。
#[derive(Debug, Clone)]
pub struct TaskVersionRecord {
    pub id: TaskVersionId,
    pub task_id: TaskId,
    pub version_no: i32,
    pub spec: DagSpec,
    pub rules: Vec<String>,
    /// 合成后规则文本的哈希，是 run 漂移指纹的一部分。
    pub rules_hash: String,
    pub created_at: DateTime<Utc>,
}

/// 建任务的入参。
#[derive(Debug, Clone)]
pub struct NewTask {
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub description: Option<String>,
    pub spec: DagSpec,
    pub rules: Vec<String>,
    pub rules_hash: String,
    pub enabled: bool,
}

impl Store {
    /// 建任务和它的首个版本。
    ///
    /// 两张表互相引用（`tasks.current_version_id` ↔ `task_versions.task_id`），
    /// 靠 FK 的 `DEFERRABLE INITIALLY DEFERRED` 在一个事务里完成。
    /// 更新任务定义：产生一个**新版本**，老版本原样保留。
    ///
    /// 历史 run 绑的是版本快照，改任务定义不能动它们——否则改完之后，
    /// 历史 run 无法解释、无法回放，漂移检测也失去了基线。
    ///
    /// `expected_version` 是乐观锁。不匹配返回 `Conflict`，调用方据此回 412。
    pub async fn update_task(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
        expected_version: Option<i64>,
        new: NewTask,
    ) -> Result<(TaskRecord, TaskVersionRecord), StoreError> {
        let version_id = TaskVersionId::new();
        let spec_json = serde_json::to_value(&new.spec).map_err(|err| StoreError::Corrupt {
            what: "DagSpec",
            detail: err.to_string(),
        })?;

        let mut tx = self.pool().begin().await?;

        // 先锁住任务行再读版本号：不加锁的话两个并发更新会读到同一个版本号，
        // 各自认为自己拿到了锁，然后后写的那个悄悄覆盖前一个。
        let current = sqlx::query(
            "SELECT version, current_version_id FROM tasks
             WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
        )
        .bind(uuid::Uuid::from(task_id))
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            what: "task",
            id: task_id.to_string(),
        })?;

        let actual: i64 = current.try_get("version")?;
        if let Some(expected) = expected_version
            && expected != actual
        {
            return Err(StoreError::Conflict {
                what: "task",
                id: format!("版本已是 {actual}，你带的 If-Match 是 {expected}"),
            });
        }

        let next_no: i32 = sqlx::query(
            "SELECT COALESCE(MAX(version_no), 0) + 1 AS n FROM task_versions WHERE task_id = $1",
        )
        .bind(uuid::Uuid::from(task_id))
        .fetch_one(&mut *tx)
        .await?
        .try_get("n")?;

        let version_row = sqlx::query(
            "INSERT INTO task_versions (id, task_id, version_no, dag_spec, rules, rules_hash)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING created_at",
        )
        .bind(uuid::Uuid::from(version_id))
        .bind(uuid::Uuid::from(task_id))
        .bind(next_no)
        .bind(&spec_json)
        .bind(&new.rules)
        .bind(&new.rules_hash)
        .fetch_one(&mut *tx)
        .await?;

        let task_row = sqlx::query(
            "UPDATE tasks
             SET name = $3, description = $4, current_version_id = $5, enabled = $6,
                 version = version + 1, updated_at = now()
             WHERE id = $1 AND workspace_id = $2
             RETURNING version, created_at, updated_at",
        )
        .bind(uuid::Uuid::from(task_id))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(&new.name)
        .bind(&new.description)
        .bind(uuid::Uuid::from(version_id))
        .bind(new.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| duplicate_name(err, &new.name))?;

        tx.commit().await?;

        Ok((
            TaskRecord {
                id: task_id,
                workspace_id,
                name: new.name,
                description: new.description,
                current_version_id: version_id,
                enabled: new.enabled,
                version: task_row.try_get("version")?,
                created_at: task_row.try_get("created_at")?,
                updated_at: task_row.try_get("updated_at")?,
            },
            TaskVersionRecord {
                id: version_id,
                task_id,
                version_no: next_no,
                spec: new.spec,
                rules: new.rules,
                rules_hash: new.rules_hash,
                created_at: version_row.try_get("created_at")?,
            },
        ))
    }

    /// 删任务。**级联删掉它的全部版本、定时配置和执行历史。**
    ///
    /// 返回会被一起删掉的 run 数，调用方要把这个数字摆到人眼前——
    /// 那些 run 里有成本记录和完整事件流，是审计材料，删了没有撤销键。
    pub async fn count_runs_of(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> Result<i64, StoreError> {
        let row =
            sqlx::query("SELECT count(*) AS n FROM runs WHERE workspace_id = $1 AND task_id = $2")
                .bind(uuid::Uuid::from(workspace_id))
                .bind(uuid::Uuid::from(task_id))
                .fetch_one(self.pool())
                .await?;
        row.try_get("n").map_err(StoreError::from)
    }

    pub async fn delete_task(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;

        // runs 会随 tasks 级联删掉，但它们的事件流和采样不会——那两张表是分区表，
        // 没有指向 runs 的外键（见 `purge_run_children` 里的理由）。不显式删的话，
        // 界面上明明写着"会同时删掉它们的完整事件流"，实际留了一堆孤儿行。
        let run_ids: Vec<uuid::Uuid> =
            sqlx::query_scalar("SELECT id FROM runs WHERE task_id = $1 AND workspace_id = $2")
                .bind(uuid::Uuid::from(task_id))
                .bind(uuid::Uuid::from(workspace_id))
                .fetch_all(&mut *tx)
                .await?;
        crate::runs::purge_run_children(&mut tx, &run_ids).await?;

        // current_version_id 指向 task_versions，而后者又 CASCADE 于 tasks：
        // 先断开这个引用，否则删除会撞上那条延迟外键。
        sqlx::query(
            "UPDATE tasks SET current_version_id = NULL WHERE id = $1 AND workspace_id = $2",
        )
        .bind(uuid::Uuid::from(task_id))
        .bind(uuid::Uuid::from(workspace_id))
        .execute(&mut *tx)
        .await?;
        let done = sqlx::query("DELETE FROM tasks WHERE id = $1 AND workspace_id = $2")
            .bind(uuid::Uuid::from(task_id))
            .bind(uuid::Uuid::from(workspace_id))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn create_task(
        &self,
        new: NewTask,
    ) -> Result<(TaskRecord, TaskVersionRecord), StoreError> {
        let task_id = TaskId::new();
        let version_id = TaskVersionId::new();
        let spec_json = serde_json::to_value(&new.spec).map_err(|err| StoreError::Corrupt {
            what: "DagSpec",
            detail: err.to_string(),
        })?;

        let mut tx = self.pool().begin().await?;

        let task_row = sqlx::query(
            "INSERT INTO tasks (id, workspace_id, name, description, current_version_id, enabled)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING version, created_at, updated_at",
        )
        .bind(uuid::Uuid::from(task_id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(&new.name)
        .bind(&new.description)
        .bind(uuid::Uuid::from(version_id))
        .bind(new.enabled)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| duplicate_name(err, &new.name))?;

        let version_row = sqlx::query(
            "INSERT INTO task_versions (id, task_id, version_no, dag_spec, rules, rules_hash)
             VALUES ($1, $2, 1, $3, $4, $5)
             RETURNING created_at",
        )
        .bind(uuid::Uuid::from(version_id))
        .bind(uuid::Uuid::from(task_id))
        .bind(&spec_json)
        .bind(&new.rules)
        .bind(&new.rules_hash)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok((
            TaskRecord {
                id: task_id,
                workspace_id: new.workspace_id,
                name: new.name,
                description: new.description,
                current_version_id: version_id,
                enabled: new.enabled,
                version: task_row.try_get("version")?,
                created_at: task_row.try_get("created_at")?,
                updated_at: task_row.try_get("updated_at")?,
            },
            TaskVersionRecord {
                id: version_id,
                task_id,
                version_no: 1,
                spec: new.spec,
                rules: new.rules,
                rules_hash: new.rules_hash,
                created_at: version_row.try_get("created_at")?,
            },
        ))
    }

    /// 按 ID 取任务。**必须带 workspace 一起查**：只按 ID 查等于把跨租户
    /// 越权做成了默认行为。
    pub async fn get_task(
        &self,
        workspace_id: WorkspaceId,
        id: TaskId,
    ) -> Result<TaskRecord, StoreError> {
        sqlx::query(
            "SELECT id, workspace_id, name, description, current_version_id, enabled,
                    version, created_at, updated_at
             FROM tasks WHERE workspace_id = $1 AND id = $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(uuid::Uuid::from(id))
        .fetch_optional(self.pool())
        .await?
        .map(task_from_row)
        .transpose()?
        .ok_or(StoreError::NotFound {
            what: "task",
            id: id.to_string(),
        })
    }

    /// 列任务，按创建时间倒序。
    pub async fn list_tasks(
        &self,
        workspace_id: WorkspaceId,
        limit: i64,
    ) -> Result<Vec<TaskRecord>, StoreError> {
        sqlx::query(
            "SELECT id, workspace_id, name, description, current_version_id, enabled,
                    version, created_at, updated_at
             FROM tasks WHERE workspace_id = $1
             ORDER BY created_at DESC, id DESC LIMIT $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(limit)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(task_from_row)
        .collect()
    }

    /// 取一个版本快照。
    pub async fn get_task_version(
        &self,
        id: TaskVersionId,
    ) -> Result<TaskVersionRecord, StoreError> {
        let row = sqlx::query(
            "SELECT id, task_id, version_no, dag_spec, rules, rules_hash, created_at
             FROM task_versions WHERE id = $1",
        )
        .bind(uuid::Uuid::from(id))
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound {
            what: "task_version",
            id: id.to_string(),
        })?;

        let spec_json: serde_json::Value = row.try_get("dag_spec")?;
        Ok(TaskVersionRecord {
            id,
            task_id: TaskId(row.try_get("task_id")?),
            version_no: row.try_get("version_no")?,
            spec: serde_json::from_value(spec_json).map_err(|err| StoreError::Corrupt {
                what: "task_versions.dag_spec",
                detail: err.to_string(),
            })?,
            rules: row.try_get("rules")?,
            rules_hash: row.try_get("rules_hash")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

fn task_from_row(row: sqlx::postgres::PgRow) -> Result<TaskRecord, StoreError> {
    Ok(TaskRecord {
        id: TaskId(row.try_get("id")?),
        workspace_id: WorkspaceId(row.try_get("workspace_id")?),
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        current_version_id: TaskVersionId(row.try_get("current_version_id")?),
        enabled: row.try_get("enabled")?,
        version: row.try_get("version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// 重名撞唯一键时给个能直接看懂的错误，而不是原样抛 23505。
fn duplicate_name(err: sqlx::Error, name: &str) -> StoreError {
    if err.as_database_error().and_then(|e| e.code()).as_deref() == Some("23505") {
        StoreError::Conflict {
            what: "task name",
            id: name.to_string(),
        }
    } else {
        StoreError::Query(err)
    }
}
