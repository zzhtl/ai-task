# stream-json 录制样本

`claude -p --output-format stream-json --verbose` 的真实输出，防腐层的回归基线。

CLI 的事件形状**不是稳定契约**。这些样本存在的意义是：升级 claude 版本后重新
录制一份，diff 一下就知道翻译层要不要跟着改，而不是等线上 run 挂了才发现。

| 文件 | 录制条件 | 说明 |
|---|---|---|
| `hermetic-count-lines.jsonl` | claude 2.1.263 / haiku-4-5 / `--strict-mcp-config --setting-sources ""` | 密闭执行。3 个工具，$0.0336 |
| `inherited-mcp-count-lines.jsonl` | 同上但不加密闭参数 | 继承了宿主的 30 个工具（含 MCP），$0.0736 |

两份的差别就是密闭执行的价值：成本降 54%，且结果不再取决于操作机上装了什么。

## 重新录制

```sh
cd <一个只有几个 .rs 文件的临时目录>
claude -p "统计当前目录下所有 .rs 文件的总行数，只回答数字" \
  --output-format stream-json --verbose --model claude-haiku-4-5 \
  --strict-mcp-config --setting-sources "" \
  --tools "Bash,Read,Glob" --permission-mode acceptEdits \
  --max-budget-usd 0.20 < /dev/null > hermetic-count-lines.jsonl
```
