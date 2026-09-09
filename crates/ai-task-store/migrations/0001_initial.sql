-- ai-task 初始 schema。
--
-- 贯穿全表的约定：
--   * 主键一律 uuid（应用侧生成 UUIDv7）。ID 出现在 URL 里、会被远端 agent 和
--     `claude --session-id` 携带，不能可枚举；UUIDv7 时间有序，不像 v4 那样打散 B-tree。
--   * 时刻一律 timestamptz，存 UTC。
--   * 枚举一律 text + CHECK，不用原生 ENUM 类型——加一个取值只是改 CHECK，
--     而 ALTER TYPE 在生产上很难受。取值必须与 ai-task-proto 的 serde tag 逐字一致。
--   * 金额一律 bigint 微美元（1 USD = 1e6）。见 ai_task_proto::UsdMicros。
--   * workspace_id 是多租户边界，作为顶层实体列表索引的**首列**。
--     子表（run_events / run_nodes / run_metrics）只挂 run_id：它们只能经由
--     一个已鉴权的 run 到达，再冗余一列 workspace_id 只会让索引变胖。
--
-- DDL 前先设 lock_timeout：等 ACCESS EXCLUSIVE 的 ALTER TABLE 会把它之后的
-- 每一条查询都堵在后面。宁可快速失败重来。
SET lock_timeout = '5s';

-- ---------------------------------------------------------------- 租户与身份

CREATE TABLE workspaces (
    id          uuid        PRIMARY KEY,
    name        text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id            uuid        PRIMARY KEY,
    workspace_id  uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    email         text        NOT NULL,
    display_name  text        NOT NULL,
    -- argon2id 的 PHC 字符串。外部身份源接入的用户为空。
    password_hash text,
    disabled_at   timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, email)
);

CREATE TABLE role_bindings (
    user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    role         text NOT NULL CHECK (role IN ('viewer', 'operator', 'admin')),
    PRIMARY KEY (user_id, workspace_id, role)
);
-- FK 子表侧索引：PG 不自动建，缺了它删父行会全表扫描并持锁。
CREATE INDEX role_bindings_workspace_idx ON role_bindings (workspace_id);

CREATE TABLE sessions (
    -- 会话 token 的 SHA-256，不存原文：库被拖走也无法冒充。
    token_hash  bytea       PRIMARY KEY,
    user_id     uuid        NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX sessions_user_idx ON sessions (user_id);
CREATE INDEX sessions_expiry_idx ON sessions (expires_at);

-- ---------------------------------------------------------------- 凭据与主机

CREATE TABLE credentials (
    id           uuid        PRIMARY KEY,
    workspace_id uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         text        NOT NULL,
    kind         text        NOT NULL CHECK (kind IN ('ssh_key', 'ssh_password', 'api_key')),
    -- chacha20poly1305 密文 + nonce。key_version 支持轮换 KEK 而不必立刻重加密全部。
    ciphertext   bytea       NOT NULL,
    nonce        bytea       NOT NULL,
    key_version  int         NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name)
);

CREATE TABLE hosts (
    id            uuid        PRIMARY KEY,
    workspace_id  uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name          text        NOT NULL,
    address       text        NOT NULL,
    port          int         NOT NULL DEFAULT 22 CHECK (port BETWEEN 1 AND 65535),
    username      text        NOT NULL,
    credential_id uuid        REFERENCES credentials(id) ON DELETE SET NULL,
    -- 策略按 tag 生效（如 prod 机上写类操作一律 ask）。
    tags          text[]      NOT NULL DEFAULT '{}',
    -- 远端能力：agent = 推送了 ai-task-agent；ssh_only = 只能跑命令，无资源归因。
    agent_mode    text        NOT NULL DEFAULT 'agent' CHECK (agent_mode IN ('agent', 'ssh_only')),
    -- 已投送的 agent 二进制哈希。与当前版本一致就跳过上传。
    agent_sha256  text,
    -- 目标机的 cgroup 能力，决定资源归因的降级程度。
    cgroup_mode   text        CHECK (cgroup_mode IN ('systemd', 'cgroup2', 'proc', 'none')),
    last_seen_at  timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name)
);
CREATE INDEX hosts_credential_idx ON hosts (credential_id);
CREATE INDEX hosts_tags_idx ON hosts USING gin (tags);

-- ---------------------------------------------------------------- Skills 与规则

CREATE TABLE skills (
    id           uuid        PRIMARY KEY,
    workspace_id uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         text        NOT NULL,
    version      text        NOT NULL,
    -- SKILL.md frontmatter 里的 description。渐进式披露时只有它进上下文。
    description  text        NOT NULL,
    body         text        NOT NULL,
    -- 附带的 references/ 等文件：路径 -> 内容。
    files        jsonb       NOT NULL DEFAULT '{}',
    -- 内容寻址：body + files 的 blake3。同内容不同版本号也能识别为同一份。
    content_hash text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name, version)
);

CREATE TABLE rules (
    id           uuid        PRIMARY KEY,
    workspace_id uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         text        NOT NULL,
    -- prompt = 软规则，注入 system prompt，只影响倾向；
    -- policy = 硬策略，在工具调用边界强制，prompt 影响不到它。
    kind         text        NOT NULL CHECK (kind IN ('prompt', 'policy')),
    -- global 的规则对本 workspace 的所有任务生效。
    scope        text        NOT NULL DEFAULT 'task' CHECK (scope IN ('global', 'task')),
    spec         jsonb       NOT NULL,
    -- 数值大的先判，首个命中生效。
    priority     int         NOT NULL DEFAULT 0,
    enabled      boolean     NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name)
);
-- 策略求值的热路径：只扫本 workspace 里启用的、按优先级排好的规则。
CREATE INDEX rules_eval_idx ON rules (workspace_id, kind, priority DESC) WHERE enabled;

-- ---------------------------------------------------------------- 任务与版本

CREATE TABLE tasks (
    id                 uuid        PRIMARY KEY,
    workspace_id       uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name               text        NOT NULL,
    description        text,
    -- 当前生效的版本。FK 延迟到事务提交时才校验，这样建任务和建首个版本
    -- 可以在同一个事务里完成（两张表互相引用）。
    current_version_id uuid,
    enabled            boolean     NOT NULL DEFAULT true,
    -- 乐观锁 / ETag。每次更新 +1，If-Match 不匹配就 409。
    version            bigint      NOT NULL DEFAULT 1,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    created_by         uuid        REFERENCES users(id) ON DELETE SET NULL,
    UNIQUE (workspace_id, name)
);
CREATE INDEX tasks_created_by_idx ON tasks (created_by);

-- 不可变快照。run 引用的是版本，不是任务。
--
-- 没有这张表，改完任务定义后历史 run 就无法解释、无法回放，漂移检测也失去基线。
CREATE TABLE task_versions (
    id           uuid        PRIMARY KEY,
    task_id      uuid        NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    version_no   int         NOT NULL,
    dag_spec     jsonb       NOT NULL,
    -- 冻结进快照的规则名列表，以及它们合成后文本的哈希。
    -- rules_hash 进 run 指纹，用来回答「是哪版规则产生了这个行为」。
    rules        text[]      NOT NULL DEFAULT '{}',
    rules_hash   text        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    created_by   uuid        REFERENCES users(id) ON DELETE SET NULL,
    UNIQUE (task_id, version_no)
);
CREATE INDEX task_versions_created_by_idx ON task_versions (created_by);

ALTER TABLE tasks
    ADD CONSTRAINT tasks_current_version_fk
    FOREIGN KEY (current_version_id) REFERENCES task_versions(id)
    DEFERRABLE INITIALLY DEFERRED;
CREATE INDEX tasks_current_version_idx ON tasks (current_version_id);

-- ---------------------------------------------------------------- 定时

CREATE TABLE schedules (
    id           uuid        PRIMARY KEY,
    workspace_id uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    task_id      uuid        NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    cron         text        NOT NULL,
    -- IANA 时区名。DST 的跳过/重复时刻由它决定，不能存固定 offset。
    timezone     text        NOT NULL DEFAULT 'UTC',
    misfire      text        NOT NULL DEFAULT 'fire_once' CHECK (misfire IN ('skip', 'fire_once', 'fire_all')),
    overlap      text        NOT NULL DEFAULT 'skip'      CHECK (overlap IN ('allow', 'skip', 'queue')),
    jitter_s     int         NOT NULL DEFAULT 0 CHECK (jitter_s >= 0),
    enabled      boolean     NOT NULL DEFAULT true,
    next_fire_at timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX schedules_task_idx ON schedules (task_id);
-- 调度器的唯一热查询：
--   SELECT id FROM schedules WHERE enabled AND next_fire_at <= now()
--   ORDER BY next_fire_at LIMIT n FOR UPDATE SKIP LOCKED
-- 部分索引让它只覆盖启用中的那一小撮。
CREATE INDEX schedules_due_idx ON schedules (next_fire_at) WHERE enabled;

-- ---------------------------------------------------------------- Run

CREATE TABLE runs (
    id              uuid        PRIMARY KEY,
    workspace_id    uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    task_id         uuid        NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    task_version_id uuid        NOT NULL REFERENCES task_versions(id),
    schedule_id     uuid        REFERENCES schedules(id) ON DELETE SET NULL,
    -- 计划触发时刻。手动/API 触发为 NULL。
    fire_at         timestamptz,
    trigger         text        NOT NULL CHECK (trigger IN ('manual', 'schedule', 'api', 'parent')),
    status          text        NOT NULL CHECK (status IN (
                        'queued', 'running', 'succeeded', 'failed',
                        'cancelled', 'timed_out', 'budget_exceeded', 'resource_exceeded')),
    -- 影子执行：副作用工具只记录意图不执行。
    dry_run         boolean     NOT NULL DEFAULT false,
    -- 与哪个基线 run 做结构化 diff。
    compare_to      uuid        REFERENCES runs(id) ON DELETE SET NULL,
    inputs          jsonb,
    output          jsonb,
    error           text,
    cost_micros     bigint      NOT NULL DEFAULT 0,
    -- 漂移指纹 = 任务版本 + 模型 + effort + 规则哈希 + skills 哈希。
    -- 同指纹但 output_digest 变了 → 行为漂移，告警。
    fingerprint     text,
    output_digest   text,
    -- 驱动本次执行的 claude CLI 版本。CLI 的 stream-json 不是稳定契约，
    -- 排查「昨天还好好的」时第一个要看的就是它。
    cli_version     text,
    -- 事件日志的最大 seq。SSE 客户端据此判断自己落后多少。
    max_seq         bigint      NOT NULL DEFAULT 0,
    created_at      timestamptz NOT NULL DEFAULT now(),
    started_at      timestamptz,
    finished_at     timestamptz,
    created_by      uuid        REFERENCES users(id) ON DELETE SET NULL,

    -- 定时触发的幂等**靠约束而不是靠锁**：多副本同时算出同一个触发点时，
    -- 只有一个能插进来，另一个撞唯一键回滚。双触发在结构上不可能。
    -- schedule_id 为 NULL 时（手动触发）约束天然不生效，多次手动触发互不影响。
    CONSTRAINT runs_schedule_fire_uniq UNIQUE (schedule_id, fire_at)
);
-- 任务详情页：某任务的 run 历史，按时间倒序。(created_at, id) 是唯一且稳定的
-- 游标排序键——单用 created_at 在同毫秒并发插入时会跳行或重复。
CREATE INDEX runs_task_recent_idx ON runs (workspace_id, task_id, created_at DESC, id DESC);
CREATE INDEX runs_recent_idx ON runs (workspace_id, created_at DESC, id DESC);
-- 引擎扫描在途 run（重启恢复、超时巡检）。部分索引：在途的永远只是一小撮。
CREATE INDEX runs_active_idx ON runs (workspace_id, created_at) WHERE status IN ('queued', 'running');
CREATE INDEX runs_version_idx ON runs (task_version_id);
CREATE INDEX runs_compare_idx ON runs (compare_to);
CREATE INDEX runs_created_by_idx ON runs (created_by);
-- 漂移检测：同一任务下同指纹的历史 run。
CREATE INDEX runs_fingerprint_idx ON runs (task_id, fingerprint, created_at DESC) WHERE fingerprint IS NOT NULL;

CREATE TABLE run_nodes (
    run_id      uuid        NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    node_key    text        NOT NULL,
    -- map 展开出的第 n 个实例；非 map 节点恒为 0。
    instance    int         NOT NULL DEFAULT 0,
    attempt     int         NOT NULL DEFAULT 1,
    status      text        NOT NULL CHECK (status IN (
                    'pending', 'ready', 'running', 'awaiting_approval',
                    'succeeded', 'failed', 'skipped', 'cancelled')),
    host_id     uuid        REFERENCES hosts(id) ON DELETE SET NULL,
    input       jsonb,
    output      jsonb,
    error       text,
    cost_micros bigint      NOT NULL DEFAULT 0,
    started_at  timestamptz,
    finished_at timestamptz,
    PRIMARY KEY (run_id, node_key, instance, attempt)
);
CREATE INDEX run_nodes_host_idx ON run_nodes (host_id);

-- ---------------------------------------------------------------- 事件日志

-- run 的状态**不是**这里的某个字段，而是这串 append-only 事件的 fold。
-- 推导逻辑只有一处：ai_task_core::RunState。
--
-- 按 ts 月度分区：老分区 DETACH + DROP 就是归档，比按行 DELETE 便宜几个数量级。
-- 分区表要求分区键必须出现在唯一约束里，所以 PK 是 (run_id, seq, ts)——
-- 前两列的顺序正好是 SSE 续传查询 `WHERE run_id = $1 AND seq > $2 ORDER BY seq`
-- 需要的索引顺序。代价：seq 的唯一性只在单个分区内强制。实际上 seq 由该 run 的
-- 单写者任务分配，跨月重复要求同一个 run 在两个月里写出同一个 seq，不会发生。
CREATE TABLE run_events (
    run_id   uuid        NOT NULL,
    seq      bigint      NOT NULL,
    ts       timestamptz NOT NULL,
    node_key text,
    -- 与 payload 里的 kind 冗余一份，供不解 jsonb 的过滤/统计使用。
    kind     text        NOT NULL,
    payload  jsonb       NOT NULL,
    PRIMARY KEY (run_id, seq, ts)
) PARTITION BY RANGE (ts);

-- 兜底分区。宁可让插入落进 DEFAULT，也不能因为维护任务没跑而让 run 直接写失败。
CREATE TABLE run_events_default PARTITION OF run_events DEFAULT;

CREATE TABLE run_metrics (
    run_id      uuid        NOT NULL,
    node_key    text        NOT NULL,
    ts          timestamptz NOT NULL,
    host_id     uuid,
    -- 归因到本 run 的 cgroup 累计量，不是机器级读数。
    cpu_usec    bigint      NOT NULL DEFAULT 0,
    rss_bytes   bigint      NOT NULL DEFAULT 0,
    io_read     bigint      NOT NULL DEFAULT 0,
    io_write    bigint      NOT NULL DEFAULT 0,
    pids        int         NOT NULL DEFAULT 0
) PARTITION BY RANGE (ts);
CREATE TABLE run_metrics_default PARTITION OF run_metrics DEFAULT;
-- 采样点没有唯一性要求（偶尔重复无害），只建查询要用的索引，
-- 省掉一个每写必更的唯一索引。
CREATE INDEX run_metrics_lookup_idx ON run_metrics (run_id, node_key, ts);

-- 确保 [今天, 今天 + months_ahead) 覆盖的月份分区都存在。
-- 由运行时每天调用一次。提前建好，DEFAULT 分区就永远是空的——
-- 这很重要：往非空 DEFAULT 分区上 ATTACH 新分区需要全扫它并持 ACCESS EXCLUSIVE 锁。
CREATE FUNCTION ai_task_ensure_partitions(months_ahead int DEFAULT 3)
RETURNS int LANGUAGE plpgsql AS $$
DECLARE
    parent text;
    start_ts date;
    end_ts   date;
    part     text;
    made     int := 0;
BEGIN
    FOREACH parent IN ARRAY ARRAY['run_events', 'run_metrics'] LOOP
        FOR i IN 0..months_ahead LOOP
            start_ts := date_trunc('month', current_date + (i || ' month')::interval);
            end_ts   := start_ts + interval '1 month';
            part     := format('%s_%s', parent, to_char(start_ts, 'YYYYMM'));
            IF to_regclass(part) IS NULL THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
                    part, parent, start_ts, end_ts);
                made := made + 1;
            END IF;
        END LOOP;
    END LOOP;
    RETURN made;
END;
$$;

SELECT ai_task_ensure_partitions();

-- ---------------------------------------------------------------- 审批与审计

CREATE TABLE approvals (
    id           uuid        PRIMARY KEY,
    run_id       uuid        NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    node_key     text,
    title        text        NOT NULL,
    -- 结构化意图：要跑的命令、要改的文件、影响的主机。
    -- 审批卡片渲染的是它，不是一段自然语言。
    intent       jsonb       NOT NULL,
    -- 触发本次审批的策略规则（effect = ask）。走 approval 节点时为 NULL。
    rule_id      uuid        REFERENCES rules(id) ON DELETE SET NULL,
    requested_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    decided_at   timestamptz,
    decided_by   uuid        REFERENCES users(id) ON DELETE SET NULL,
    approved     boolean,
    reason       text
);
-- 待决审批的巡检（超时自动拒绝）。
CREATE INDEX approvals_pending_idx ON approvals (expires_at) WHERE decided_at IS NULL;
CREATE INDEX approvals_run_idx ON approvals (run_id);
CREATE INDEX approvals_rule_idx ON approvals (rule_id);
CREATE INDEX approvals_decided_by_idx ON approvals (decided_by);

-- 创建/触发类 POST 的幂等键。
--
-- 关键点：响应必须与副作用在**同一个事务**里写入，否则重放会返回一个
-- 与实际发生的事情不一致的结果。
CREATE TABLE idempotency_keys (
    workspace_id uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    key          text        NOT NULL,
    -- 请求体的哈希。同 key 不同 body 必须报 409，而不是把旧响应当成新结果返回。
    request_hash text        NOT NULL,
    status_code  int         NOT NULL,
    response     jsonb       NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- 过期窗口要长于任何客户端的重试周期，24h 是下限。
    expires_at   timestamptz NOT NULL,
    PRIMARY KEY (workspace_id, key)
);
CREATE INDEX idempotency_keys_expiry_idx ON idempotency_keys (expires_at);

CREATE TABLE audit_log (
    id           bigserial   PRIMARY KEY,
    workspace_id uuid        NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    actor_id     uuid        REFERENCES users(id) ON DELETE SET NULL,
    action       text        NOT NULL,
    target_kind  text        NOT NULL,
    target_id    text        NOT NULL,
    before       jsonb,
    after        jsonb,
    request_id   text,
    ts           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_recent_idx ON audit_log (workspace_id, ts DESC);
CREATE INDEX audit_log_target_idx ON audit_log (workspace_id, target_kind, target_id, ts DESC);
CREATE INDEX audit_log_actor_idx ON audit_log (actor_id);
