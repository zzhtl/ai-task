SET lock_timeout = '5s';

-- `run_nodes` 整表零读写：没有任何 Rust 代码碰过它。
-- `dag.rs` 的注释说 map 扇出的每个实例在这里各占一行，实际从来没写过。
--
-- 死表比缺功能更危险：下一个人会以为它有数据，照着它写查询，
-- 然后得到一个永远为空的结果，而且不报错。ADR 0001 已经定死了
-- 事件日志是唯一的真相来源；要节点级视图的话，正确做法是从事件流投影出来。
DROP TABLE IF EXISTS run_nodes;

-- io_read / io_write 从建表起就没被写过：agent 只采 cpu_usec / rss_bytes / pids。
-- DROP COLUMN 在 Postgres 里是元数据操作，不重写数据，只短暂持 ACCESS EXCLUSIVE，
-- 上面的 lock_timeout 足够兜住。
ALTER TABLE run_metrics DROP COLUMN IF EXISTS io_read;
ALTER TABLE run_metrics DROP COLUMN IF EXISTS io_write;
