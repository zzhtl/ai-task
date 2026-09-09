# 0004. 用 Claude Code CLI 作为执行内核，并把它关在防腐层后面

## 状态

已接受（2026-09-08）。

## 决策

开放式 AI 节点由 `claude -p --output-format stream-json --verbose` 驱动，
所有对 CLI 输出的认知集中在 `ai-task-exec::claude_code::decode` 一个模块里，
对上只暴露自定义的 `ExecEvent`。

**执行一律密闭。** 每次调用固定带三个隔离开关：

| 开关 | 作用 |
|---|---|
| `--strict-mcp-config` | 不读宿主的 MCP 配置 |
| `--setting-sources ""` | 不加载 user / project / local 的 settings.json |
| `--no-session-persistence` | 会话不落宿主磁盘 |

默认工具白名单只有 `Read,Glob,Grep`——写入类工具要节点显式声明，
免得「忘了配」等于「默认能改文件」。

## 取舍

**为什么不自建 Messages API 循环。** CLI 白送的东西自己实现要几个月：完整工具集、
上下文自动压缩、`PreToolUse` hook（M2 的策略拦截点）、`--max-budget-usd` 成本熔断、
`--json-schema` 输出契约、`--session-id`/`--resume` 可恢复会话。自建 loop 保留给
封闭式推理节点（assert、分类、抽取），那里不需要文件和命令工具，用 Haiku 更便宜。

**代价：`stream-json` 不是稳定契约。** 字段会加、会改、会重排。对策是防腐层
加录制回归：`tests/fixtures/` 下是真实录制的 JSONL，`decode_fixtures.rs`
回放它们并断言**不产生任何 Warning**——升级 CLI 后重录一份再跑测试，
第一时间就知道翻译层要不要改。`runs.cli_version` 记下每次执行的版本，
排查「昨天还好好的」时第一个看它。

**密闭执行的代价是隔离带来的能力损失**（宿主装的 MCP 工具用不上），
换来的是可复现与省钱。实测同一个任务：

| | 可用工具 | 成本 |
|---|---|---|
| 继承宿主配置 | 30 个（含 22 个 MCP 工具） | $0.0736 |
| 密闭执行 | 3 个 | $0.0336 |

便宜 54%，但更重要的是**结果不再取决于操作机上恰好装了什么**。
定时任务在不同机器上跑出不同结果，是最难查的一类问题。

## 两个实测出来的坑

**1. 逐条 `assistant` 事件里的 usage 不能用来算钱。**
同一次执行里照单累加 `output_tokens` 得 16，按 `message.id` 去重得 8，
而 `result` 事件的权威值是 **694**——差两个数量级。逐条 usage 是流式过程中的
局部快照。所以整个执行只发一条 `Usage` 事件，取自 `result`。
拿逐条 usage 算成本会让预算熔断形同虚设。

**2. 预算耗尽时 `usage` 整个是 0。**
`--max-budget-usd` 命中时 CLI 返回 `result/error_max_budget_usd`，
此时 `usage` 全零，只有 `modelUsage` 和 `total_cost_usd` 还是对的。
所以 token 数取 `modelUsage` 的汇总，不取 `usage`——否则被熔断的 run
会显示成本为零，等于把已经花掉的钱从账上抹掉。

两条都由 `decode_fixtures.rs` 里的断言钉住。
