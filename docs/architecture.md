# 架构

ai-task 是一个自托管的 **AI 执行控制平面**：把「一次 AI 执行」当作可观测、
可回放、可审计、可策略拦截、可预算约束的一等生产实体来管理。DAG 编排、cron
定时、SSH 分发都是围绕这个内核的外壳。

## 设计原则

1. **Run 是事件日志，不是状态字段。** 状态推导只有一处实现
   （`ai_task_core::RunState`）；调度器、API、前端回放走的都是它，因此不会出现
   「界面显示成功、数据库写着失败」这种分叉。见 [ADR 0001](adr/0001-event-sourced-runs.md)。
2. **策略在执行边界强制，不在 prompt 里祈求。** 软规则（注入 system prompt）
   只影响倾向；硬策略在每次工具调用前拦截。工具结果是不可信输入，能影响模型
   对 prompt 的遵守，所以策略层必须在 prompt 之外。
3. **契约优先于约定。** 节点声明输出 schema，校验失败把错误回喂给模型重试；
   未知字段拒绝而不是忽略；枚举取值在数据库里由 `CHECK` 兜住。
4. **失败要可见。** 缺失的静态资源真 404 而不是回退到 SPA；打错的接口路径返回
   JSON 404 而不是 HTML；缺 bun 时构建硬失败而不是产出空壳二进制。
5. **零 IO 的核心。** `ai-task-core` 不碰数据库、网络、文件系统和时钟，
   全部逻辑用普通单测覆盖。

## 依赖方向

```
                    ai-task-server  (axum 入口 / SSE / 静态资源)
                           │
                    ai-task-runtime (cron 调度 / DAG 引擎 / 策略引擎)
                     ┌─────┼─────┐
                     ▼     ▼     ▼
             ai-task-exec  │  ai-task-mcp     ← 远端工具代理
        (CLI + API 双内核) │  (rmcp server)
                     └─────┼─────┘
                           ▼
                    ai-task-store   (sqlx / migrations)
                           ▼
                    ai-task-core    (领域逻辑，零 IO)
                           ▼
                    ai-task-proto   (线上类型 + ts-rs 导出)

        ai-task-agent  ← 独立二进制，投送到目标机，只依赖 proto
```

箭头是**允许**的依赖方向，反向依赖是错误。特别地：`ai-task-runtime` 不得
另起一套状态推导，必须用 `ai_task_core::RunState`。

## 各 crate 职责

| crate | 职责 | 状态 |
|---|---|---|
| `ai-task-proto` | 前后端共享的线上类型；`RunEvent`、`DagSpec`、API DTO。ts-rs 导出 TS | M0 完成 |
| `ai-task-core` | DAG 结构校验与拓扑序、Run 状态机。**零 IO** | M0 完成 |
| `ai-task-store` | PostgreSQL 持久化。**整个仓库只有这里写 SQL** | M0 完成 |
| `ai-task-server` | axum 路由、错误契约、请求 ID、SSE、内嵌前端 | M1 完成 |
| `ai-task-exec` | `Executor` trait + Claude Code CLI 内核；`stream-json` 的防腐层 | M1 完成（API 内核在 M3） |
| `ai-task-runtime` | run 生命周期、事件写入、崩溃收尾、cron 调度器、DAG 引擎 | M3 完成 |
| `ai-task-mcp` | 远端工具代理（rmcp server），所有远端动作的唯一入口 | ✅ |
| `ai-task-agent` | 目标机上的 musl 静态二进制：cgroup 内执行 + 资源采样 | ✅ |
| `web/` | SvelteKit 2 + Svelte 5 + Vite 6 + bun，产物由 rust-embed 内嵌 | M0 完成（骨架） |

## 前端

纯 SPA，没有 Node 运行时。`bun run build` 产物落在
`crates/ai-task-server/dist/`，由 `rust-embed` 内嵌进二进制——最终交付是**一个
可执行文件**。开发期 Vite 代理 `/api` 到后端，前后端同源，不依赖 CORS。

缓存策略跟着 SvelteKit 的产物布局走：`_app/immutable/` 下文件名带内容哈希，
长期强缓存；`index.html` 每次校验，否则改版后用户一直拿到旧壳。

类型不手写，由 ts-rs 从 Rust 生成，见 [ADR 0003](adr/0003-generated-frontend-types.md)。

## 相关文档

- [ADR 0001 — Run 建模为事件日志](adr/0001-event-sourced-runs.md)
- [ADR 0002 — HTTP 契约](adr/0002-api-contract.md)
- [ADR 0003 — 前后端共享类型由 ts-rs 生成](adr/0003-generated-frontend-types.md)
- [ADR 0004 — 用 Claude Code CLI 作为执行内核](adr/0004-claude-code-as-execution-kernel.md)
- [ADR 0005 — cron 库选 croner](adr/0005-cron-library.md)
- [ADR 0006 — 规则分两层：软规则进 prompt，硬策略在工具调用边界](adr/0006-policy-layer.md)
- [ADR 0007 — DAG 执行：map 是节点内扇出，就绪判定看入边](adr/0007-dag-execution.md)
- [ADR 0008 — 远端执行：推送一个 agent，凭据留在中心](adr/0008-remote-execution.md)
- [ADR 0009 — 远端工具代理：模型的每一次远端动作都要回中心](adr/0009-remote-tool-proxy.md)
- [ADR 0010 — 审批、预算与漂移：让定时 AI 任务能进生产](adr/0010-approval-budget-drift.md)
