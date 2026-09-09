-- run_events / run_metrics 是分区表，**没有指向 runs 的外键**。
--
-- 不是疏漏：给事件表加外键意味着每插一条事件多一次父行检查，而那是这个系统
-- 最热的写路径（1Hz 采样 × N 节点 + 每次工具调用若干条）。代价是级联删除要
-- 手写——见 store 里的 `purge_run_children`，所有删 run 的路径都必须走它。
--
-- 在此之前删任务只删掉了 runs，事件和采样留了下来：它们的 run_id 指向不存在的
-- 行，任何读路径都按 run_id 过滤，所以这些行**永远读不到，也永远不会变小**。
-- 这里一次性清掉存量。删的是不可达数据，没有任何查询会因此少看到东西。
DELETE FROM run_metrics m WHERE NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = m.run_id);
DELETE FROM run_events e WHERE NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = e.run_id);
