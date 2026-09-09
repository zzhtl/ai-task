# 0006. 规则分两层：软规则进 prompt，硬策略在工具调用边界

## 状态

已接受（2026-09-08）。

## 决策

- **软规则（prompt）**：合成后注入 `--append-system-prompt`，只影响模型倾向。
- **硬策略（policy）**：`PreToolUse` hook 在每次工具调用前回调
  `POST /internal/policy/decide`，判决 allow / deny / ask。

把两者混为一谈是这类系统最容易犯的致命错误。工具结果是不可信输入，它能影响
模型对 prompt 的遵守程度；策略层跑在模型之外，**上下文里的任何内容都影响不到它**。

策略表达式只做声明式匹配（工具名 + 参数上的 regex/glob/包含/相等），
不上通用脚本语言：规则要可审计、可静态分析，而且规则本身写不出 RCE。
正则在**保存规则时**就试编译一次，坏表达式当场 422 拒掉。

## 三个实测出来的坑

### 1. hook 失败会被当成放行 —— fail-open

第一版 hook 每次调用都 panic（`main` 是 `#[tokio::main]`，而 hook 里用
`reqwest::blocking`，它自带的运行时在异步上下文里 drop 会炸）。
**Claude Code 把 hook 崩溃当成「无决策」，放行了**，而且事件日志上完全看不出来
——策略层整个失效却毫无迹象。

对策有三层：
- `main` 不再是 `#[tokio::main]`，hook 在建立运行时之前处理
- hook **任何**失败路径都输出一条合法的 deny（连不上、超时、解析不了、
  序列化失败），并且永远以退出码 0 结束
- `tests/hook_binary.rs` 真的**执行二进制**去验这些路径。单测覆盖不到
  「进程起来就崩」

### 2. `--setting-sources ""` 会连带屏蔽技能包发现

为了密闭执行给了空值，结果 `.claude/skills/` 也不被扫描了——技能包落了地却
从来没被用上。改成 `--setting-sources project`：只读工作目录自己的
`.claude/`，那是每个 run 新建的、内容完全由平台写入。实测确认它既能发现我们
写入的技能包，又不会把操作机上的个人 skill 和 MCP server 拉进来。

**前提**：`AI_TASK_WORKDIR_ROOT` 不能放在一个自带 `.claude/` 的仓库里。

另外，勾了技能包必须同时把 `Skill` 工具加进 `--tools`——否则包落了地模型也调
不动，表现是「配了但没生效」。

### 3. 单条正则不构成删除策略

配了 `\brm\s+-[a-zA-Z]*[rR]` 之后让模型删一个目录，第一次 `rm -rf junk` 被
精确拦下，**模型随即改用 `find junk -type f -delete` 绕过去，目录被删了**。

这是 denylist 的根本问题：对手会换写法，而它比你能想到的写法多。管用的形状是
**默认拒绝该工具，再用更高优先级的规则放行明确列出的用法**：

```jsonc
// 优先级 10：兜底拒绝
{ "name": "P-000-默认拒绝所有 shell", "priority": 10, "effect": "deny",
  "match": { "tool": "Bash" } }          // 不写 any_of = 匹配该工具的所有调用

// 优先级 200：白名单放行
{ "name": "P-001-放行只读命令", "priority": 200, "effect": "allow",
  "match": { "tool": "Bash", "arg": "command",
             "any_of": [{ "regex": "^\\s*(ls|cat|wc|df)\\b" }] } }
```

加上兜底拒绝之后重跑同一个任务，模型试了一次就放弃，目录保住了。

这条教训进了 `policy.rs` 的测试
（`a_rule_without_patterns_matches_every_call_of_that_tool`、
`an_allowlist_rule_at_higher_priority_reopens_specific_usages`），也应该进文档：
**给用户的默认模板必须是白名单形状的**。

## 其它取舍

**未知工具按写类处理**（fail closed）。新增一个能改东西的工具却忘了更新
`READ_ONLY_TOOLS`，后果必须是「被拦下」而不是「默认放行」。

**`ask` 在 M5 之前一律按拒绝处理**，并在理由里说清「人工审批尚未上线」。
静默放行会让「生产机上的写操作需要确认」这条默认策略变成摆设。

**不缓存规则。** 每次工具调用都从库里读一遍——策略是活的护栏，改了就该立刻
生效。一次工具调用背后是几秒的模型时间，多一次查询是噪音。

**内部接口有令牌**，每次启动随机生成，只写进各个 run 工作目录里的 hook 配置。
比较用常数时间，短路比较会通过耗时泄漏前缀。没有它，同机上的任何进程都能替
run 批准工具调用。
