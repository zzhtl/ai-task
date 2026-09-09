# 0001. Run 建模为事件日志，而不是状态字段

## 状态

已接受（2026-09-08）。

## 决策

一次 run 的状态**不存**成 `runs.status` 这样一个可变字段，而存成 `run_events`
里 append-only 的事件序列；当前状态是这串事件的 fold，推导逻辑只有一处实现
（`ai_task_core::RunState`）。`runs.status` 仍然存在，但它是**投影**，用于列表
查询和调度器扫描，不是真值来源。

配套的存储决策：

- **PostgreSQL 16+ / sqlx 0.9**。JSONB 存事件负载与 DAG spec；
  `FOR UPDATE SKIP LOCKED` 当队列（不引 Redis / Kafka）；`LISTEN`/`NOTIFY` 做
  跨副本的事件扇出。
- `run_events` 与 `run_metrics` 按 `ts` **月度 range 分区**。归档 = `DETACH` +
  `DROP`，不是按行 `DELETE`。`ai_task_ensure_partitions()` 每天提前建好未来
  三个月，DEFAULT 分区只作兜底。
- `run_events` 主键 `(run_id, seq, ts)`。分区表要求分区键出现在唯一约束里，
  而前两列的顺序正好是续传查询需要的索引顺序。
- **资源采样不进事件日志**。1Hz × N 节点会把它撑爆，走独立的 `run_metrics`。
- 主键一律 **UUIDv7**（应用侧生成）。ID 出现在 URL 里、会被远端 agent 和
  `claude --session-id` 携带，不能可枚举；v7 时间有序，不像 v4 那样打散 B-tree。
- 枚举一律 `text` + `CHECK`，取值与 `ai-task-proto` 的 serde tag 逐字一致。
- 金额一律 `bigint` 微美元。绝不用浮点，也不引任意精度小数库——定价的最小
  粒度就是「每 token 多少微美元」，整数可精确表示。
- 定时触发的幂等**靠约束不靠锁**：`runs` 上 `UNIQUE (schedule_id, fire_at)`。

## 取舍

**换来什么。** 实时监控直接 tail 事件流（不轮询）；时间轴可以回放任意时刻的
完整状态；审计天然完备（谁在什么时候让 AI 做了什么、调了什么工具、被哪条策略
拦了）；崩溃恢复靠重放而不是 checkpoint。这四件事如果分别实现，成本远高于
一次性把 Run 建成事件流。

**付出什么。** 一次 run 几百到几千条事件，写放大明显；查询要先 fold；
`agent_text` 这类流式增量占了事件量的大头却对状态推导没有贡献。缓解手段：
`RunEventBody::is_stream_delta()` 标出这类事件供归档时降采样；`RunState`
**不累积**流式文本（前端自己在 SSE 上拼），否则服务端内存会随输出长度线性增长。

**分区的代价。** `seq` 的唯一性只在单个分区内强制。跨月重复要求同一个 run 在
两个月里写出同一个 seq——seq 由该 run 的单写者任务分配，不会发生。用这个换来
了近乎免费的归档。

**为什么不上 Temporal。** 运维重量对不上收益，Postgres 在「单实例每天几万
run」这个量级完全够。**重新评估的信号**：`run_events` 总行数超过约 5 亿，
或者需要跨机房。

**为什么不 SQLite。** 多副本、`LISTEN`/`NOTIFY`、原生分区，三样都要。

**一条硬约束。** 不要把 PgBouncer 以 transaction 模式挡在前面，它会破坏
`LISTEN` 的会话语义。需要池代理时用 session 模式。

## 验证

`crates/ai-task-store/tests/schema.rs` 针对真实 PostgreSQL 16 锁住了这些不变量。
建库时用 psql 实测过的执行计划（100k runs / 100k events）：

| 查询 | 计划 | 代价 |
|---|---|---|
| SSE 续传 `run_id = ? AND seq > ?` | Merge Append + 每分区 Index Scan | 218 buffers / 0.53 ms |
| 游标分页 `(created_at, id) < ?` | Index Only Scan | 6 buffers / 0.16 ms |
| 同样深度用 `OFFSET 50000` | Index Only Scan 但要跳过 50k 行 | 1732 buffers / 15.3 ms |
| 调度器领取（部分索引） | Index Scan + LockRows | 0.056 ms |

空分区每个只多花 2 个 buffer，分区没有伤到热查询。深翻页的 288 倍差距是
「一律游标分页、不提供 OFFSET」这条契约的依据。
