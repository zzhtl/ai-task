# 0003. 前后端共享类型由 ts-rs 生成，不手写

## 状态

已接受（2026-09-08）。**这是对本仓库既有惯例的一次刻意偏离。**

## 决策

`ai-task-proto` 的所有线上类型用 `ts-rs` 导出到 `web/src/lib/api/types/`，
生成的 `.ts` **提交进仓库**。

- 导出目录由 `.cargo/config.toml` 里的 `TS_RS_EXPORT_DIR` 指定，
  所以 Rust 侧只需要裸的 `#[ts(export)]`
- 生成命令就是 `cargo test -p ai-task-proto`
- CI 跑一次生成后 `git diff --exit-code`，改了 Rust 类型却忘了重新生成会直接失败

## 取舍

**为什么偏离。** 仓库现状是手写 Rust struct ↔ TS interface（见
`dev-tools/web/src/lib/api.ts`），对那些项目是对的：类型少、形状简单。
本项目不同——`RunEventBody` 一个枚举就有 18 个 variant，两侧必须逐字段对齐，
而漂移的表现是「实时监控界面某类事件静默不显示」，单元测试抓不到。
`DagSpec` 同理。类型基数够大时，生成比手写便宜。

**代价。** 多一个依赖；生成走 `cargo test` 这个略别扭的入口；`.ts` 进仓库
会在 review 里产生噪音 diff。

**为什么提交生成产物而不是在 CI 里现生成。** CI 是「前端 job → artifact →
后端 job」的顺序，前端 job 不装 Rust 工具链。提交产物让前端能独立构建，
代价用一条 `git diff --exit-code` 检查抵消。

**为什么不上 OpenAPI + codegen。** 多一层 spec 要维护，而目前没有第三方
消费者需要 spec。有外部 API 需求时再引 `utoipa`，那时 spec 是产出物而不是
中间层。

## 已知的不完美

带 `#[serde(default)]` 但没有 `skip_serializing_if` 的字段（`retry`、
`on_failure`）在 TS 侧是必填的：服务端总会序列化它们，所以读路径正确；
但客户端构造 spec 时会被要求填。真要修得拆出独立的写侧类型，会让类型面翻倍，
目前不值得。

`ts-rs` 认不得 `#[serde(deny_unknown_fields)]`（那只影响反序列化，与 TS 类型
无关），会为每个用到它的结构体报一次 warning。已用 `no-serde-warnings` feature
关掉。真正的形状保证由 `ai-task-proto` 里的 JSON 断言测试提供，不依赖这些
warning——例如「可选字段被省略而不是序列化成 null」「枚举是 snake_case 字符串」
「金额是字符串不是数字」。
