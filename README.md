# ai-task

AI 执行控制平面：把「一次 AI 执行」当作可观测、可回放、可审计、可策略拦截、
可预算约束的一等生产实体来管理。定时或一次性触发，DAG 编排，可下发到任意
远程主机，实时监控执行过程与它对目标机的资源影响。

Rust（axum）+ Svelte 5 / SvelteKit + bun，交付物是**一个可执行文件**。

## 快速开始

```sh
# 1. 起数据库
docker compose -f deploy/docker-compose.yml up -d

# 2. 构建并运行（build.rs 会自动调 bun 打前端并内嵌）
cargo run -p ai-task-server

# 3. 打开 http://127.0.0.1:8930
```

前端单独开发时：

```sh
cd web && bun run dev     # 5173，/api 代理到 8930
```

## 验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 需要真实 PostgreSQL 的集成测试（不设这个变量会跳过）
AI_TASK_TEST_DATABASE_URL=postgres://ai_task@127.0.0.1:55432/postgres \
  cargo test --workspace

# 真跑一次 claude 的端到端测试（会花钱，约 $0.03，默认跳过）
AI_TASK_LIVE_CLAUDE=1 cargo test -p ai-task-exec --test live_claude -- --nocapture

cd web && bun run check
```

改了 `ai-task-proto` 里的线上类型后，重新生成前端类型：

```sh
cargo test -p ai-task-proto     # 产物写入 web/src/lib/api/types/，需要提交
```

## 配置

全部参数既是 CLI 选项也是环境变量（`ai-task --help`）：

| 环境变量 | 默认值 | 说明 |
|---|---|---|
| `AI_TASK_LISTEN` | `127.0.0.1:8930` | 监听地址。**默认只监听回环**——这个服务能 SSH 到任意机器执行命令，对外暴露必须是显式选择 |
| `AI_TASK_DATABASE_URL` | `postgres://ai_task@127.0.0.1:55432/ai_task` | PostgreSQL 连接串 |
| `AI_TASK_DB_MAX_CONNECTIONS` | CPU 核数 × 2 | 池上限。不要往大调 |
| `AI_TASK_AUTO_MIGRATE` | `true` | 启动时自动跑迁移 |
| `AI_TASK_LOG` | `info,ai_task=debug,tower_http=warn,sqlx=warn` | 日志过滤 |
| `AI_TASK_CLAUDE_BINARY` | `claude` | claude 可执行文件；不在 PATH 里时给绝对路径 |
| `AI_TASK_WORKDIR_ROOT` | `/tmp/ai-task/runs` | run 工作目录的根，每个 run 一个子目录 |
| `AI_TASK_WORKER_ID` | `worker-1` | 本副本标识，进事件日志。多副本要给不同值 |
| `AI_TASK_SCHEDULER_INTERVAL_MS` | `1000` | 调度器轮询间隔，决定定时任务最多晚多久触发 |
| `AI_TASK_ENCRYPTION_KEY` | 无，**必填** | 凭据加密密钥，64 位十六进制。缺了拒绝启动：主机私钥必须加密落库，没有安全的降级选项。生成：`openssl rand -hex 32` |
| `AI_TASK_LOCAL_AGENT` | `ai-task-agent` | 本机 agent 二进制。shell 节点在本机也走 agent，这样本机执行同样有资源归因和上限 |
| `AI_TASK_REMOTE_AGENT` | 无 | 推送到远端的 musl 静态二进制。不配的话远端节点会明确报错，不会静默降级 |
| `AI_TASK_KNOWN_HOSTS` | `~/.ssh/known_hosts` | SSH known_hosts |
| `AI_TASK_SSH_ACCEPT_NEW` | `true` | 首次见到新主机时记下它的密钥（TOFU）。**密钥变了永远是拒绝**，这个开关只影响没见过的主机 |
| `AI_TASK_MAX_CONCURRENT_RUNS` | CPU 核数（封在 1–32） | 同时最多跑几个 run。每个 run 会拉起一个 `claude` 子进程——吃 CPU、吃内存、**花钱**。超出上限的 run 留在 `queued` 排队，不拒绝 |
| `AI_TASK_REQUIRE_AUTH` | `false` | 是否强制登录。默认只监听回环，单人自托管不必先建账号；**`--listen` 一旦离开回环而没开这个，进程拒绝启动** |

`AI_TASK_WORKDIR_ROOT` **不要放在带 `.claude/` 的仓库里**：执行时读 project
作用域的配置，那个仓库的设置会被当成 run 自己的读进来。

## 文档

- [架构](docs/architecture.md)
- [ADR 0001 — Run 建模为事件日志](docs/adr/0001-event-sourced-runs.md)
- [ADR 0002 — HTTP 契约](docs/adr/0002-api-contract.md)
- [ADR 0003 — 前后端共享类型由 ts-rs 生成](docs/adr/0003-generated-frontend-types.md)
- [ADR 0008 — 远端执行：推送一个 agent，凭据留在中心](docs/adr/0008-remote-execution.md)
- [ADR 0009 — 远端工具代理：模型的每一次远端动作都要回中心](docs/adr/0009-remote-tool-proxy.md)
- [ADR 0010 — 审批、预算与漂移：让定时 AI 任务能进生产](docs/adr/0010-approval-budget-drift.md)

## 当前进度

**M0（骨架与契约）+ M1（执行内核与事件流）+ M2（调度、规则、策略、Skills）+ M3（DAG 编排）+ M4（远端执行与资源归因）+ M5（审批、预算、漂移、RBAC）已完成。**

现在可以：建任务（DAG 在入库时校验）→ 触发 → Claude Code 密闭执行 →
事件流实时推到前端（首个事件 < 0.1s）→ run 状态完全由事件日志重建。
断线按 `Last-Event-ID` 续传，服务被 kill 后重启会收尾中断的 run 并保住已花的钱。

还可以：配 cron 定时（支持 5/6 段、`L`、`#`、IANA 时区与 DST，
misfire / overlap / jitter 策略齐全，多副本靠 `UNIQUE(schedule_id, fire_at)` 保证
一个触发点只产生一个 run）；配软规则注入 prompt、硬策略在工具调用边界强制拦截；
装技能包供任务勾选。

还可以：画 DAG（条件边、重试并把校验错误回喂给模型、`map` 按上游数组扇出、
`assert` 节点），在网页上看编排图并叠加实时执行状态——分层布局手写，零前端依赖，
点图上的节点跳到执行过程里对应的那一段。

还可以：把任务下发到任意机器。SSH 推一个 570 KB 的 musl 静态二进制过去
（目标机上没有 Node、没有 API key），命令在 cgroup scope 里跑，
CPU/RSS/进程数按 run 归因、1 Hz 边采边落库，界面上和编排图、事件流共享一根时间轴。
`MemoryMax` 超限被内核 kill 时 run 落 `resource_exceeded`，与普通失败分开。
目标机没有 systemd 就降级到 `/proc` 采样，界面上明确标出「上限未强制」。
AI 节点也能在远端干活：它拿到的是 `remote_bash`/`remote_read`/`remote_write`/`remote_glob`，
本地文件工具全部去掉，每次调用都先过策略。

还可以：卡审批门（`approval` 节点，或策略判 `ask` 时挂起那次工具调用），
卡片上是结构化意图而不是一段自然语言，**超时默认拒绝**；
按 run 设 USD 预算，超了就不再启动新节点并标 `budget_exceeded`；
盯行为漂移——每个 run 记输入指纹和输出摘要，**输入没变而输出变了**才告警，
换 prompt 前可以跑影子执行（`dry_run` + `compare_to`）拿到与基线的逐字段 diff，
且目标机上不会发生任何事；登录、三档角色、以及一条记录"谁做了什么"的审计流水。

还可以：管人——加账号、改角色（立刻生效，不用重新登录）、停用（连带吊销会话）、
口令泄漏时把人踢下线；**最后一个管理员摘不掉**，因为摘掉之后只能改库救。

后续：漂移告警的外部推送通道、用户自助改口令、单设备粒度的会话管理。
见 `docs/architecture.md`。

> **影子执行的"无副作用"要在每一类节点上单独兑现。** `dry_run` 原本只在策略层
> 实现，而 `shell` / `approval` 由引擎直接执行、绕开了那一层——实测撞到过一次
> 影子 run 真的在目标机上写了文件。见 [ADR 0010](docs/adr/0010-approval-budget-drift.md)。

> **远端 AI 节点必须去掉本地工具。** 留着的话模型会在中心机上老老实实执行
> `Bash`/`Write`，而它以为自己在操作目标机——没有报错，只有做错了的事。
> 见 [ADR 0009](docs/adr/0009-remote-tool-proxy.md)。

> **策略要写成白名单形状。** 实测：只配一条 `rm -r` 的正则，模型会改用
> `find -delete` 绕过去。正确做法是「默认拒绝该工具 + 高优先级放行清单」，
> 见 [ADR 0006](docs/adr/0006-policy-layer.md)。

### 试一下

```sh
docker compose -f deploy/docker-compose.yml up -d
cargo run -p ai-task-server
# 打开 http://127.0.0.1:8930 ，点"新建示例任务"再点"运行"
```

一次示例执行约 $0.015（Haiku 4.5）。
