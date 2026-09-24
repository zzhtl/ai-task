// 原始事件的一行描述。排查时看的是这一行，所以**不能丢信息，也不能崩**：
// 认不出的事件类型原样输出 JSON。

import type { RunEvent } from '$api/types/RunEvent';
import { triggerLabel } from '$lib/ui/format';

export function describeEvent(event: RunEvent): string {
  const b = event.body;
  switch (b.kind) {
    case 'run_queued':
      return `已入队（${triggerLabel(b.trigger)}${b.dry_run ? '，影子执行' : ''}）`;
    case 'run_started':
      return `worker ${b.worker} 开始执行`;
    case 'run_finished':
      return `结束：${b.status}${b.error ? ` — ${b.error}` : ''}`;
    case 'node_ready':
      return '依赖就绪';
    case 'node_started':
      return `开始（第 ${b.attempt} 次尝试）`;
    case 'node_finished':
      return `节点结束：${b.status}${b.error ? ` — ${b.error}` : ''}`;
    case 'node_retrying':
      return `第 ${b.attempt} 次失败，${b.delay_ms}ms 后重试：${b.reason}`;
    case 'node_skipped':
      return `跳过：${b.reason}`;
    case 'drift_detected':
      return `行为漂移：输入条件没变，${b.changed_paths.join('、')} 变了（基线 ${b.baseline_run_id.slice(0, 8)}）`;
    case 'resource_degraded':
      return `目标机只能按 ${b.mode} 记账，资源上限未强制${b.detail ? ` — ${b.detail}` : ''}`;
    case 'map_expanded':
      return `展开 ${b.count} 个实例（上游共 ${b.available} 项）`;
    case 'agent_text':
    case 'agent_thinking':
      return b.text;
    // 提示词里的换行会把这一行撑成一屏；完整命令在执行过程的「执行命令」里
    case 'agent_invoked': {
      const flat = b.command.replace(/\s+/g, ' ');
      return `启动 AI：${flat.slice(0, 96)}${flat.length > 96 ? '…' : ''}`;
    }
    case 'agent_turn_started':
      return `第 ${b.turn} 轮`;
    case 'tool_requested':
      return `调用 ${b.tool} ${JSON.stringify(b.input).slice(0, 160)}`;
    case 'tool_completed':
      return `${b.ok ? '完成' : '失败'}：${b.output_preview}`;
    case 'policy_decided':
      return `策略 ${b.effect}：${b.reason}`;
    case 'usage':
      return `${b.model} · 入 ${b.input_tokens} / 出 ${b.output_tokens} · $${b.cost_usd}`;
    case 'approval_requested':
      return `等待审批：${b.title}`;
    // 决策人为空不代表超时：没开认证时人工决策也没有名字，是不是超时看原因
    case 'approval_decided':
      return `审批${b.approved ? '通过' : '拒绝'}${b.decided_by ? `（${b.decided_by}）` : ''}${b.reason ? `：${b.reason}` : ''}`;
    case 'log':
      return b.message;
    default:
      return JSON.stringify(b);
  }
}
