-- 目标机上探测到的 AI CLI。界面据此决定"这台机器能不能选"。
--
-- 存快照而不是每次现探：探测要连一次 SSH，而界面上列主机是高频操作。
-- 快照会过期（机器上装了新东西），所以真正执行前还会再验一次。
ALTER TABLE hosts ADD COLUMN ai_clis jsonb NOT NULL DEFAULT '[]'::jsonb;
