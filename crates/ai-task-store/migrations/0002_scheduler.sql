-- 调度器需要把「规范触发点」和「实际领取时刻」分开存。
--
-- 抖动（jitter）是为了避免整点惊群，但它**不能**污染 `next_fire_at`：
-- 后者会原样写进 `runs.fire_at`，而 `UNIQUE (schedule_id, fire_at)` 正是
-- 靠它保证同一个触发点只产生一个 run。加了随机量之后多副本各自算出的
-- fire_at 会不同，幂等直接失效。
--
-- 所以：`next_fire_at` 是规范触发点（进 runs.fire_at），
-- `next_claim_at` = 规范触发点 + 抖动（调度器比较的是它）。
SET lock_timeout = '5s';

ALTER TABLE schedules ADD COLUMN next_claim_at timestamptz;

-- 已有行：没有抖动，两者相同
UPDATE schedules SET next_claim_at = next_fire_at WHERE next_claim_at IS NULL;

-- 调度器的唯一热查询改成按 next_claim_at 扫。
-- 旧索引留着没用了，一并换掉。
DROP INDEX IF EXISTS schedules_due_idx;
CREATE INDEX schedules_due_idx ON schedules (next_claim_at) WHERE enabled;

-- 上一次实际产生 run 的规范触发点。misfire 补偿要从它开始往后算，
-- 否则服务停了三天再起来，无从知道错过了哪些。
ALTER TABLE schedules ADD COLUMN last_fired_at timestamptz;

-- 领取时用它防止同一副本在一次轮询里重复处理，也便于排查卡住的调度。
ALTER TABLE schedules ADD COLUMN last_claimed_at timestamptz;
