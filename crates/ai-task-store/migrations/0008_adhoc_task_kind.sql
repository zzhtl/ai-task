SET lock_timeout = '5s';

-- 临时批量执行命令也要落成 run：出现在执行记录里、能看每台机器的结果、有资源采样和审计。
-- run 必须挂在某个任务版本下，所以每个 workspace 有一个系统任务（kind = 'adhoc'），
-- 每次临时执行给它插一个新版本——那一版就是"那次到底跑了什么命令、在哪些机器上"。
--
-- 系统任务不出现在任务列表里，也不该占用户的名字空间：原来的 (workspace_id, name)
-- 唯一约束换成只管普通任务的部分唯一索引。另一个部分唯一索引保证每个 workspace
-- 最多一个系统任务，并发的第一次临时执行靠它 ON CONFLICT DO NOTHING。
--
-- ADD COLUMN 带常量默认值只改元数据、不重写表；tasks 只有百行级，
-- 建索引和 CHECK 的全表校验都在毫秒级。
--
-- 回滚：DROP 两个新索引；重建 tasks_workspace_id_name_key（前提是没有和系统任务
-- 重名的普通任务）；DROP COLUMN kind。系统任务和它下面的 run 要先删掉。

ALTER TABLE tasks
    ADD COLUMN kind text NOT NULL DEFAULT 'task'
        CONSTRAINT tasks_kind_check CHECK (kind IN ('task', 'adhoc'));

ALTER TABLE tasks DROP CONSTRAINT tasks_workspace_id_name_key;

CREATE UNIQUE INDEX tasks_workspace_name_uniq ON tasks (workspace_id, name) WHERE kind = 'task';

CREATE UNIQUE INDEX tasks_one_adhoc_per_workspace ON tasks (workspace_id) WHERE kind = 'adhoc';
