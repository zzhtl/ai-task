// 把事件流折成「一次 AI 执行到底发生了什么」。
//
// 原始事件流是给排查用的：一条一行、按 seq 排、什么都不合并。但要看懂一次执行
// 得在里面来回对照——tool_requested 在第 8 条、策略判决在第 9 条、结果在第 10 条，
// 三条讲的是同一件事；`agent_thinking` 干脆被整个过滤掉了。
//
// 这里做三件事：**按节点分组**（一步一段）、**把讲同一件事的事件合成一块**
// （工具调用的请求/判决/结果合成一张卡）、**把连续的增量拼回整段话**。
//
// 纯函数，不碰 DOM：这是整个页面里唯一有逻辑的部分，必须能单测。

import type { NodeStatus } from '$api/types/NodeStatus';
import type { RunEvent } from '$api/types/RunEvent';

export type PolicyEffect = 'allow' | 'deny' | 'ask';

export type Block =
  /** 拉起 AI 的命令行。 */
  | { kind: 'command'; seq: number; command: string; ranOn: string | null }
  | { kind: 'turn'; seq: number; turn: number }
  | { kind: 'thinking'; seq: number; text: string }
  | { kind: 'say'; seq: number; text: string }
  | {
      kind: 'tool';
      seq: number;
      id: string;
      tool: string;
      input: unknown;
      /** `null` = 还没判决（正在跑，或者这条路径没有策略层）。 */
      effect: PolicyEffect | null;
      reason: string | null;
      /** `null` = 还没结束。 */
      ok: boolean | null;
      preview: string | null;
      durationMs: number | null;
    }
  | {
      kind: 'gate';
      seq: number;
      title: string;
      /** `null` = 还在等人。 */
      approved: boolean | null;
      by: string | null;
      reason: string | null;
    }
  | { kind: 'note'; seq: number; level: string; text: string }
  | { kind: 'retry'; seq: number; attempt: number; delayMs: number; reason: string; fedBack: boolean }
  | {
      kind: 'usage';
      seq: number;
      model: string;
      inTok: number;
      outTok: number;
      cacheRead: number;
      costUsd: string;
    };

export interface NodeRun {
  key: string;
  /** 编排里的展示名。查不到就用 key。 */
  name: string;
  status: NodeStatus | null;
  startedAt: string | null;
  finishedAt: string | null;
  attempt: number;
  error: string | null;
  output: unknown;
  /** 本节点累计花费（美元）。 */
  costUsd: number;
  blocks: Block[];
}

/** 会说话的两类块可以往前拼——模型的输出是按增量来的。 */
const MERGEABLE = new Set(['thinking', 'say']);

/**
 * 事件流 → 按节点分好组的执行过程。
 *
 * 节点顺序按**第一次出现**排，不按编排里的顺序：并行分支下，谁先跑起来
 * 谁先出现，这比照着 spec 的静态顺序更贴近"实际发生了什么"。
 */
export function groupProcess(events: RunEvent[], names: Record<string, string>): NodeRun[] {
  const nodes = new Map<string, NodeRun>();
  /** tool_use_id → 那张卡，好让后到的判决和结果填进同一张。 */
  const tools = new Map<string, Extract<Block, { kind: 'tool' }>>();

  const nodeOf = (key: string): NodeRun => {
    let node = nodes.get(key);
    if (!node) {
      node = {
        key,
        name: names[key] ?? key,
        status: null,
        startedAt: null,
        finishedAt: null,
        attempt: 1,
        error: null,
        output: null,
        costUsd: 0,
        blocks: []
      };
      nodes.set(key, node);
    }
    return node;
  };

  const push = (node: NodeRun, block: Block) => {
    const last = node.blocks.at(-1);
    // 连续的增量拼回整段话，不然一段回答会被拆成十几个孤立的块
    if (last && last.kind === block.kind && MERGEABLE.has(block.kind)) {
      (last as { text: string }).text += (block as { text: string }).text;
      return;
    }
    node.blocks.push(block);
  };

  for (const event of events) {
    const b = event.body;
    const seq = event.seq;
    // run 级事件（入队 / 开始 / 结束）在页头已经写着了，这里只讲节点里发生的事
    if (!event.node_key) continue;
    const node = nodeOf(event.node_key);

    switch (b.kind) {
      case 'node_started':
        node.status = 'running';
        node.attempt = b.attempt;
        node.startedAt ??= event.ts;
        break;
      case 'node_ready':
        node.status ??= 'ready';
        break;
      case 'node_finished':
        node.status = b.status;
        node.finishedAt = event.ts;
        node.error = b.error ?? null;
        node.output = b.output ?? null;
        break;
      case 'node_skipped':
        node.status = 'skipped';
        push(node, { kind: 'note', seq, level: 'info', text: `跳过：${b.reason}` });
        break;
      case 'node_retrying':
        push(node, {
          kind: 'retry',
          seq,
          attempt: b.attempt,
          delayMs: b.delay_ms,
          reason: b.reason,
          fedBack: b.feeding_error_to_model
        });
        break;

      case 'agent_invoked':
        push(node, { kind: 'command', seq, command: b.command, ranOn: b.ran_on ?? null });
        break;
      case 'agent_turn_started':
        push(node, { kind: 'turn', seq, turn: b.turn });
        break;
      case 'agent_thinking':
        push(node, { kind: 'thinking', seq, text: b.text });
        break;
      case 'agent_text':
        push(node, { kind: 'say', seq, text: b.text });
        break;

      case 'tool_requested': {
        const card: Extract<Block, { kind: 'tool' }> = {
          kind: 'tool',
          seq,
          id: b.tool_use_id,
          tool: b.tool,
          input: b.input,
          effect: null,
          reason: null,
          ok: null,
          preview: null,
          durationMs: null
        };
        tools.set(b.tool_use_id, card);
        node.blocks.push(card);
        break;
      }
      case 'policy_decided': {
        const card = tools.get(b.tool_use_id);
        if (card) {
          card.effect = b.effect as PolicyEffect;
          card.reason = b.reason;
        } else {
          // 配不上请求的判决说明事件流不完整。让它可见，别静默丢掉
          push(node, {
            kind: 'note',
            seq,
            level: b.effect === 'deny' ? 'warn' : 'info',
            text: `策略 ${b.effect}：${b.reason}`
          });
        }
        break;
      }
      case 'tool_completed': {
        const card = tools.get(b.tool_use_id);
        if (card) {
          card.ok = b.ok;
          card.preview = b.output_preview;
          card.durationMs = Number(b.duration_ms);
        } else {
          push(node, {
            kind: 'note',
            seq,
            level: b.ok ? 'info' : 'warn',
            text: `工具结束：${b.output_preview}`
          });
        }
        break;
      }

      case 'approval_requested':
        push(node, { kind: 'gate', seq, title: b.title, approved: null, by: null, reason: null });
        break;
      case 'approval_decided': {
        const gate = [...node.blocks].reverse().find((x) => x.kind === 'gate' && x.approved === null);
        if (gate && gate.kind === 'gate') {
          gate.approved = b.approved;
          gate.by = b.decided_by ?? null;
          gate.reason = b.reason ?? null;
        }
        break;
      }

      case 'usage':
        node.costUsd += Number(b.cost_usd);
        push(node, {
          kind: 'usage',
          seq,
          model: b.model,
          inTok: Number(b.input_tokens),
          outTok: Number(b.output_tokens),
          cacheRead: Number(b.cache_read_tokens),
          costUsd: String(b.cost_usd)
        });
        break;

      case 'resource_degraded':
        push(node, {
          kind: 'note',
          seq,
          level: 'warn',
          // 上限没被强制这件事必须说出来，不然人会以为 limits 生效了
          text: `目标机只能按 ${b.mode} 记账，资源上限未强制${b.detail ? ` — ${b.detail}` : ''}`
        });
        break;
      case 'drift_detected':
        push(node, {
          kind: 'note',
          seq,
          level: 'warn',
          text: `行为漂移：输入条件没变，${b.changed_paths.join('、')} 变了`
        });
        break;
      case 'map_expanded':
        push(node, {
          kind: 'note',
          seq,
          level: 'info',
          text: `展开 ${b.count} 个实例（上游共 ${b.available} 项）`
        });
        break;
      case 'log':
        push(node, { kind: 'note', seq, level: b.level, text: b.message });
        break;
      default:
        break;
    }
  }

  return [...nodes.values()];
}

/** 编排 → 节点 key 到展示名。画布上叫什么，过程里就叫什么。 */
export function nodeNames(spec: { nodes: Array<{ key: string; name?: string | null }> } | null) {
  const out: Record<string, string> = {};
  for (const node of spec?.nodes ?? []) out[node.key] = node.name || node.key;
  return out;
}
