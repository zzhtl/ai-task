SET lock_timeout = '5s';

-- 归档：把超过保留期的月度分区 DETACH 掉再 DROP。
--
-- `0001_initial.sql` 的注释里早就写着「老分区 DETACH + DROP 就是归档，
-- 比按行 DELETE 便宜几个数量级」——设计一直有，只是没人实现，
-- 于是分区数每月 +1 且没有上限。分区数无界会让 SSE 那条
-- `WHERE run_id = $1 AND seq > $2` 的 Merge Append 越扫越多张表。
--
-- DETACH CONCURRENTLY 不持 ACCESS EXCLUSIVE，不会把正在写事件的 run 卡住。
-- 它不能在事务块里跑，所以这个函数里每张表单独一句、失败就跳过下一轮再来。
CREATE FUNCTION ai_task_drop_old_partitions(retain_months int DEFAULT 6)
RETURNS int LANGUAGE plpgsql AS $$
DECLARE
    parent  text;
    part    record;
    cutoff  date;
    dropped int := 0;
BEGIN
    cutoff := date_trunc('month', current_date - (retain_months || ' month')::interval);

    FOREACH parent IN ARRAY ARRAY['run_events', 'run_metrics'] LOOP
        FOR part IN
            SELECT c.relname AS name
            FROM pg_class c
            JOIN pg_inherits i ON i.inhrelid = c.oid
            JOIN pg_class p ON p.oid = i.inhparent
            WHERE p.relname = parent
              -- 只碰 YYYYMM 结尾的月度分区；DEFAULT 兜底分区永远保留
              AND c.relname ~ ('^' || parent || '_[0-9]{6}$')
              AND to_date(right(c.relname, 6), 'YYYYMM') < cutoff
        LOOP
            EXECUTE format('ALTER TABLE %I DETACH PARTITION %I', parent, part.name);
            EXECUTE format('DROP TABLE %I', part.name);
            dropped := dropped + 1;
        END LOOP;
    END LOOP;

    RETURN dropped;
END;
$$;
