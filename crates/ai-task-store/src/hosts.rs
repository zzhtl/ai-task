//! 目标主机与资源采样。

use ai_task_proto::{HostId, RunId, WorkspaceId};
use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Store, StoreError};

/// 一台可下发任务的目标机。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub id: HostId,
    pub name: String,
    pub address: String,
    pub port: i32,
    pub username: String,
    /// 策略按 tag 生效（如 prod 机上写类操作一律 ask）。
    pub tags: Vec<String>,
    /// 上次投送的 agent 二进制哈希。一致就跳过上传。
    pub agent_sha256: Option<String>,
    /// 上次探测到的资源归因档位。降级了界面上要标出来。
    pub cgroup_mode: Option<String>,
    /// 上次探测到的 AI CLI。**是快照，会过期**——真正执行前还会再验一次。
    pub ai_clis: serde_json::Value,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewHost {
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub address: String,
    pub port: i32,
    pub username: String,
    pub tags: Vec<String>,
    /// SSH 私钥。会加密后存进 credentials 表。
    pub private_key: String,
}

/// 改主机的入参。`private_key` 为 `None` 表示钥匙不动。
///
/// 编辑表单里永远看不到旧钥匙（读接口不返回它），所以"没填"必须是"不换"，
/// 不能是"清掉"。
#[derive(Debug, Clone)]
pub struct HostUpdate {
    pub name: String,
    pub address: String,
    pub port: i32,
    pub username: String,
    pub tags: Vec<String>,
    pub private_key: Option<String>,
}

/// 一条资源采样点。
#[derive(Debug, Clone, Copy)]
pub struct MetricSample {
    pub at: DateTime<Utc>,
    /// 累计 CPU 微秒。是累计量，前端做差得到速率。
    pub cpu_usec: i64,
    pub rss_bytes: i64,
    pub pids: i32,
}

impl Store {
    pub async fn create_host(&self, new: NewHost) -> Result<HostId, StoreError> {
        let host_id = HostId::new();
        let credential_id = ai_task_proto::CredentialId::new();
        let mut tx = self.pool().begin().await?;

        // 私钥加密存。这里先落一版明文占位是不可接受的——所以哪怕 M4 还没接
        // KMS，也用一个进程级 KEK 把它包起来（见 crypto 模块）。
        let sealed = crate::crypto::seal(new.private_key.as_bytes());
        sqlx::query(
            "INSERT INTO credentials (id, workspace_id, name, kind, ciphertext, nonce, key_version)
             VALUES ($1, $2, $3, 'ssh_key', $4, $5, $6)",
        )
        .bind(uuid::Uuid::from(credential_id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(format!("{}-ssh-key", new.name))
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(sealed.key_version)
        .execute(&mut *tx)
        .await
        .map_err(|err| duplicate(err, "host credential", &new.name))?;

        sqlx::query(
            "INSERT INTO hosts (id, workspace_id, name, address, port, username, credential_id, tags)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(uuid::Uuid::from(host_id))
        .bind(uuid::Uuid::from(new.workspace_id))
        .bind(&new.name)
        .bind(&new.address)
        .bind(new.port)
        .bind(&new.username)
        .bind(uuid::Uuid::from(credential_id))
        .bind(&new.tags)
        .execute(&mut *tx)
        .await
        .map_err(|err| duplicate(err, "host name", &new.name))?;

        tx.commit().await?;
        Ok(host_id)
    }

    /// 改一台主机。返回 `false` 表示这台主机不在这个 workspace 里。
    pub async fn update_host(
        &self,
        workspace_id: WorkspaceId,
        id: HostId,
        update: HostUpdate,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT credential_id FROM hosts WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let credential_id: Option<uuid::Uuid> = row.try_get("credential_id")?;

        sqlx::query(
            "UPDATE hosts SET name = $3, address = $4, port = $5, username = $6, tags = $7
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .bind(&update.name)
        .bind(&update.address)
        .bind(update.port)
        .bind(&update.username)
        .bind(&update.tags)
        .execute(&mut *tx)
        .await
        .map_err(|err| duplicate(err, "host name", &update.name))?;

        // 凭据的名字跟着主机名走：不跟的话，改完名再建一台原名的主机会撞唯一约束
        let credential_name = format!("{}-ssh-key", update.name);
        match (update.private_key.as_deref(), credential_id) {
            (Some(key), Some(cred)) => {
                let sealed = crate::crypto::seal(key.as_bytes());
                sqlx::query(
                    "UPDATE credentials
                     SET name = $2, ciphertext = $3, nonce = $4, key_version = $5
                     WHERE id = $1",
                )
                .bind(cred)
                .bind(&credential_name)
                .bind(&sealed.ciphertext)
                .bind(&sealed.nonce)
                .bind(sealed.key_version)
                .execute(&mut *tx)
                .await
                .map_err(|err| duplicate(err, "host credential", &update.name))?;
            }
            (Some(key), None) => {
                let cred = ai_task_proto::CredentialId::new();
                let sealed = crate::crypto::seal(key.as_bytes());
                sqlx::query(
                    "INSERT INTO credentials
                       (id, workspace_id, name, kind, ciphertext, nonce, key_version)
                     VALUES ($1, $2, $3, 'ssh_key', $4, $5, $6)",
                )
                .bind(uuid::Uuid::from(cred))
                .bind(uuid::Uuid::from(workspace_id))
                .bind(&credential_name)
                .bind(&sealed.ciphertext)
                .bind(&sealed.nonce)
                .bind(sealed.key_version)
                .execute(&mut *tx)
                .await
                .map_err(|err| duplicate(err, "host credential", &update.name))?;
                sqlx::query("UPDATE hosts SET credential_id = $2 WHERE id = $1")
                    .bind(uuid::Uuid::from(id))
                    .bind(uuid::Uuid::from(cred))
                    .execute(&mut *tx)
                    .await?;
            }
            (None, Some(cred)) => {
                sqlx::query("UPDATE credentials SET name = $2 WHERE id = $1")
                    .bind(cred)
                    .bind(&credential_name)
                    .execute(&mut *tx)
                    .await
                    .map_err(|err| duplicate(err, "host credential", &update.name))?;
            }
            (None, None) => {}
        }

        tx.commit().await?;
        Ok(true)
    }

    /// 删一台主机，连同它的 SSH 凭据。返回 `false` 表示不存在。
    ///
    /// 这里不查有没有任务还指着它——调用方先用 [`Store::tasks_using_host`] 问清楚，
    /// 删除本身该是纯粹的。
    pub async fn delete_host(
        &self,
        workspace_id: WorkspaceId,
        id: HostId,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(
            "DELETE FROM hosts WHERE id = $1 AND workspace_id = $2 RETURNING credential_id",
        )
        .bind(uuid::Uuid::from(id))
        .bind(uuid::Uuid::from(workspace_id))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        if let Some(cred) = row.try_get::<Option<uuid::Uuid>, _>("credential_id")? {
            sqlx::query("DELETE FROM credentials WHERE id = $1")
                .bind(cred)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// 当前版本里有步骤钉在这台主机上的任务名，按名字排。
    ///
    /// 只看当前版本：老版本指着它没关系，新 run 绑的是当前版本。
    pub async fn tasks_using_host(
        &self,
        workspace_id: WorkspaceId,
        id: HostId,
    ) -> Result<Vec<String>, StoreError> {
        let rows = sqlx::query(
            r#"SELECT t.name FROM tasks t
               JOIN task_versions v ON v.id = t.current_version_id
               WHERE t.workspace_id = $1
                 AND jsonb_path_exists(
                       v.dag_spec,
                       '$.nodes[*].host ? (@.on == "host" && @.host_id == $id)',
                       jsonb_build_object('id', $2::text))
               ORDER BY t.name"#,
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(id.to_string())
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|row| row.try_get("name").map_err(StoreError::from))
            .collect()
    }

    pub async fn get_host(
        &self,
        workspace_id: WorkspaceId,
        id: HostId,
    ) -> Result<(Host, String), StoreError> {
        let row = sqlx::query(
            "SELECT h.id, h.name, h.address, h.port, h.username, h.tags, h.agent_sha256, h.ai_clis,
                    h.cgroup_mode, h.last_seen_at, c.ciphertext, c.nonce, c.key_version
             FROM hosts h
             LEFT JOIN credentials c ON c.id = h.credential_id
             WHERE h.workspace_id = $1 AND h.id = $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(uuid::Uuid::from(id))
        .fetch_optional(self.pool())
        .await?
        .ok_or(StoreError::NotFound {
            what: "host",
            id: id.to_string(),
        })?;

        let ciphertext: Option<Vec<u8>> = row.try_get("ciphertext")?;
        let nonce: Option<Vec<u8>> = row.try_get("nonce")?;
        let key_version: Option<i32> = row.try_get("key_version")?;
        let key = match (ciphertext, nonce, key_version) {
            (Some(ciphertext), Some(nonce), Some(version)) => {
                crate::crypto::open(&ciphertext, &nonce, version).ok_or(StoreError::Corrupt {
                    what: "credentials.ciphertext",
                    detail: "私钥解密失败，可能是 KEK 变了".into(),
                })?
            }
            _ => {
                return Err(StoreError::NotFound {
                    what: "host credential",
                    id: id.to_string(),
                });
            }
        };

        Ok((
            host_from_row(&row)?,
            String::from_utf8_lossy(&key).to_string(),
        ))
    }

    /// 只取 tag。策略判决每次工具调用都要用，不该为此拉出整行加解密私钥。
    ///
    /// `Ok(None)` 表示这台主机不在这个 workspace 里——调用方必须把它当成
    /// 拒绝，而不是"没有 tag"。
    pub async fn host_tags(
        &self,
        workspace_id: WorkspaceId,
        host_id: HostId,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let row = sqlx::query("SELECT tags FROM hosts WHERE id = $1 AND workspace_id = $2")
            .bind(uuid::Uuid::from(host_id))
            .bind(uuid::Uuid::from(workspace_id))
            .fetch_optional(self.pool())
            .await?;
        row.map(|row| row.try_get("tags").map_err(StoreError::from))
            .transpose()
    }

    pub async fn list_hosts(&self, workspace_id: WorkspaceId) -> Result<Vec<Host>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, name, address, port, username, tags, agent_sha256, ai_clis, cgroup_mode,
                    last_seen_at
             FROM hosts WHERE workspace_id = $1 ORDER BY name
             LIMIT $2",
        )
        .bind(uuid::Uuid::from(workspace_id))
        .bind(crate::pool::CONFIG_LIST_CAP)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(host_from_row).collect()
    }

    /// 记下这次连上时探到的能力。界面靠它标出降级的主机。
    pub async fn record_host_probe(
        &self,
        id: HostId,
        agent_sha256: &str,
        cgroup_mode: &str,
        ai_clis: &serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE hosts SET agent_sha256 = $2, cgroup_mode = $3, ai_clis = $4,
                    last_seen_at = now()
             WHERE id = $1",
        )
        .bind(uuid::Uuid::from(id))
        .bind(agent_sha256)
        .bind(cgroup_mode)
        .bind(ai_clis)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// 批量写资源采样点。
    ///
    /// **不进 `run_events`**：1Hz × N 节点会把事件日志撑爆，而这些点对状态推导
    /// 没有任何贡献。单独一张按时间分区的表，归档时直接 DETACH。
    pub async fn append_metrics(
        &self,
        run_id: RunId,
        node_key: &str,
        host_id: Option<HostId>,
        samples: &[MetricSample],
    ) -> Result<(), StoreError> {
        if samples.is_empty() {
            return Ok(());
        }
        let ts: Vec<DateTime<Utc>> = samples.iter().map(|s| s.at).collect();
        let cpu: Vec<i64> = samples.iter().map(|s| s.cpu_usec).collect();
        let rss: Vec<i64> = samples.iter().map(|s| s.rss_bytes).collect();
        let pids: Vec<i32> = samples.iter().map(|s| s.pids).collect();

        sqlx::query(
            "INSERT INTO run_metrics (run_id, node_key, ts, host_id, cpu_usec, rss_bytes, pids)
             SELECT $1, $2, t, $3, c, r, p
             FROM UNNEST($4::timestamptz[], $5::bigint[], $6::bigint[], $7::int[]) AS s(t, c, r, p)",
        )
        .bind(uuid::Uuid::from(run_id))
        .bind(node_key)
        .bind(host_id.map(uuid::Uuid::from))
        .bind(&ts)
        .bind(&cpu)
        .bind(&rss)
        .bind(&pids)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// 读一个 run 的资源曲线。
    pub async fn read_metrics(
        &self,
        run_id: RunId,
    ) -> Result<Vec<(String, MetricSample)>, StoreError> {
        let rows = sqlx::query(
            "SELECT node_key, ts, cpu_usec, rss_bytes, pids
             FROM run_metrics WHERE run_id = $1 ORDER BY ts",
        )
        .bind(uuid::Uuid::from(run_id))
        .fetch_all(self.pool())
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("node_key")?,
                    MetricSample {
                        at: row.try_get("ts")?,
                        cpu_usec: row.try_get("cpu_usec")?,
                        rss_bytes: row.try_get("rss_bytes")?,
                        pids: row.try_get("pids")?,
                    },
                ))
            })
            .collect()
    }
}

fn host_from_row(row: &sqlx::postgres::PgRow) -> Result<Host, StoreError> {
    Ok(Host {
        id: HostId(row.try_get("id")?),
        name: row.try_get("name")?,
        address: row.try_get("address")?,
        port: row.try_get("port")?,
        username: row.try_get("username")?,
        tags: row.try_get("tags")?,
        agent_sha256: row.try_get("agent_sha256")?,
        cgroup_mode: row.try_get("cgroup_mode")?,
        ai_clis: row.try_get("ai_clis")?,
        last_seen_at: row.try_get("last_seen_at")?,
    })
}

fn duplicate(err: sqlx::Error, what: &'static str, id: &str) -> StoreError {
    if err.as_database_error().and_then(|e| e.code()).as_deref() == Some("23505") {
        StoreError::Conflict {
            what,
            id: id.to_string(),
        }
    } else {
        StoreError::Query(err)
    }
}
