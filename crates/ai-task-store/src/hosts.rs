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

    pub async fn get_host(
        &self,
        workspace_id: WorkspaceId,
        id: HostId,
    ) -> Result<(Host, String), StoreError> {
        let row = sqlx::query(
            "SELECT h.id, h.name, h.address, h.port, h.username, h.tags, h.agent_sha256,
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
            "SELECT id, name, address, port, username, tags, agent_sha256, cgroup_mode, last_seen_at
             FROM hosts WHERE workspace_id = $1 ORDER BY name",
        )
        .bind(uuid::Uuid::from(workspace_id))
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
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE hosts SET agent_sha256 = $2, cgroup_mode = $3, last_seen_at = now()
             WHERE id = $1",
        )
        .bind(uuid::Uuid::from(id))
        .bind(agent_sha256)
        .bind(cgroup_mode)
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
