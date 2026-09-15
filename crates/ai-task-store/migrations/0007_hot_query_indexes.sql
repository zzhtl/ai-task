SET lock_timeout = '5s';

-- 两条热查询的索引。
--
-- **没用 `CREATE INDEX CONCURRENTLY`**，虽然 sqlx 文档上说 `-- no-transaction`
-- 能关掉迁移事务。实测在 sqlx 0.9 的 `migrate!` 宏路径上这个指令没生效
-- （仍然报 `CREATE INDEX CONCURRENTLY cannot run inside a transaction block`），
-- 没有深究。走普通 CREATE INDEX 的理由本来也成立：
--
--   * 这是单人自托管的控制平面，建索引期间短暂阻塞写入可以接受；
--   * 上面的 lock_timeout 保证阻塞不会变成长时间卡死；
--   * CONCURRENTLY 失败会留下一个 `indisvalid = false` 的索引——
--     它不报错、也永远不被使用，比短暂阻塞更难发现。
--
-- 将来真变成 24/7 有写入的共享实例，就把建索引挪出迁移、
-- 用 psql 手工 CONCURRENTLY 建好，再让迁移里的 IF NOT EXISTS 变成空操作。

-- 按状态筛执行记录。现有索引只有 (workspace_id, created_at DESC, id DESC)
-- 和一个只覆盖 queued/running 的部分索引，所以 `?status=failed` 只能全扫再过滤，
-- 而且状态越罕见扫得越多。
CREATE INDEX IF NOT EXISTS runs_status_recent_idx
    ON runs (workspace_id, status, created_at DESC, id DESC);

-- 资源采样按时间读。现有索引是 (run_id, node_key, ts)，
-- 中间隔着 node_key，满足不了 `WHERE run_id = $1 ORDER BY ts`——
-- 出来的顺序是按节点分组的，还得再排一次。
--
-- run_metrics 是分区表。Postgres 会把父表索引自动传播到之后新建的分区，
-- 所以 ai_task_ensure_partitions 不用改。
CREATE INDEX IF NOT EXISTS run_metrics_run_ts_idx ON run_metrics (run_id, ts);
