// 把事件流折成「一次 AI 执行到底发生了什么」。
//
// 原始事件流是给排查用的：一条一行、按 seq 排、什么都不合并。但要看懂一次执行
// 得在里面来回对照——tool_requested 在第 8 条、策略判决在第 9 条、结果在第 10 条，
// 三条讲的是同一件事。
//
// 这里做三件事：**按节点分组**（一步一段）、**把讲同一件事的事件合成一块**
// （工具调用的请求/判决/结果合成一张卡）、**把连续的增量拼回整段话**。
//
// **增量的。**一次长执行有上万条事件（模型输出是按字流过来的），以前每来一条就把
// 全部事件从头折一遍——O(n²)，跑得越久页面越卡。现在每条事件只动它碰到的那一块：
// 块对象一旦产出就不再原地改，要改就换一个新对象；`snapshot()` 只给变了的节点出新对象，
// 没变的节点、没变的块沿用上一次的引用——界面按引用判断，只重画变了的那一段。
//
// 纯逻辑，不碰 DOM，也不用 rune：这是执行详情里唯一有逻辑的部分，必须能单测。

import type { NodeStatus } from '$api/types/NodeStatus';
import type { RunEvent } from '$api/types/RunEvent';
import { toMicros } from '$lib/ui/format';

export type PolicyEffect = 'allow' | 'deny' | 'ask';

export type ToolBlock = {
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
  /** run 已经结束而这次调用始终没等到结果（被取消、被中断）。不能一直显示"执行中"。 */
  ended: boolean;
};

export type GateBlock = {
  kind: 'gate';
  seq: number;
  /** 决策事件靠它找回这张卡。按位置找会配错：同一步里可能挂着好几张。 */
  approvalId: string;
  title: string;
  /** `null` = 还在等人。 */
  approved: boolean | null;
  /**
   * 谁做的决定。**空值不等于超时**：没开认证时人工决策也拿不到名字，
   * 是不是超时看 `reason`。
   */
  by: string | null;
  reason: string | null;
  /** run 已经结束而这张卡始终没有结论。 */
  ended: boolean;
};

export type Block =
  /** 拉起 AI 的命令行。 */
  | { kind: 'command'; seq: number; command: string; ranOn: string | null }
  | { kind: 'turn'; seq: number; turn: number }
  | { kind: 'thinking'; seq: number; text: string }
  | { kind: 'say'; seq: number; text: string }
  | ToolBlock
  | GateBlock
  | { kind: 'note'; seq: number; level: string; text: string; detail?: string }
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

/**
 * 节点状态，外加一个只在界面上存在的 `interrupted`：run 已经结束，
 * 这一步却没等到 node_finished（重启回收、执行器失联）。它不会再有结论了。
 */
export type NodeRunStatus = NodeStatus | 'interrupted';

/** 还没有结论：没开始、就绪，或者正在跑。 */
export const isOpen = (status: string | null | undefined): boolean =>
  status == null || status === 'pending' || status === 'ready' || status === 'running';

export interface NodeRun {
  key: string;
  /** 编排里的展示名。查不到就用 key。 */
  name: string;
  status: NodeRunStatus | null;
  startedAt: string | null;
  finishedAt: string | null;
  attempt: number;
  error: string | null;
  output: unknown;
  /** 本节点累计花费，整数微美元。累加用整数：浮点加几十次就对不上账了。 */
  costMicros: number;
  blocks: Block[];
}

/** 步骤导航的一行：编排里的每一步，跑过的带上状态、耗时和花费。 */
export interface NavStep {
  key: string;
  name: string;
  status: string;
  startedAt: string | null;
  finishedAt: string | null;
  costMicros: number;
}

/** 会说话的两类块可以往前拼——模型的输出是按增量来的。 */
const MERGEABLE = new Set(['thinking', 'say']);

/** 某一块在哪：哪个节点的第几块。块会被整个换掉，所以不能直接存块对象。 */
interface Slot {
  node: NodeRun;
  index: number;
}

export class ProcessBuilder {
  private names: Record<string, string>;
  private readonly nodes = new Map<string, NodeRun>();
  /** 节点第一次出现的先后。并行分支下谁先跑起来谁先出现，比编排里的静态顺序更贴近实际。 */
  private readonly order: string[] = [];
  /** tool_use_id → 那张卡，好让后到的判决和结果填进同一张。 */
  private readonly tools = new Map<string, Slot>();
  /** approval_id → 那张审批卡。 */
  private readonly gates = new Map<string, Slot>();
  private readonly dirty = new Set<string>();
  private readonly snapshots = new Map<string, NodeRun>();

  constructor(names: Record<string, string> = {}) {
    this.names = names;
  }

  /** 展示名可能晚到（版本快照是异步取的）。换了之后所有节点重新出快照。 */
  setNames(names: Record<string, string>): void {
    this.names = names;
    for (const [key, node] of this.nodes) {
      node.name = names[key] ?? key;
      this.dirty.add(key);
    }
  }

  /** 自上次 `snapshot()` 以来有没有变化。 */
  get changed(): boolean {
    return this.dirty.size > 0;
  }

  push(event: RunEvent): void {
    const b = event.body;
    const seq = event.seq;

    // 审批结论按 approval_id 找卡，不看它挂在哪个节点下：策略 ask 的结论
    // 曾经写成 run 级事件（没有 node_key），按节点找会找不到，卡片永远停在"等人点头"。
    // 同一个结论可能被写了两次（hook 重试），按 id 覆盖是幂等的。
    if (b.kind === 'approval_decided') {
      const slot = this.gates.get(b.approval_id);
      if (slot) {
        this.patch<GateBlock>(slot, {
          approved: b.approved,
          by: b.decided_by ?? null,
          reason: b.reason ?? null
        });
        return;
      }
    }
    // run 结束时还没有结论的，不能一直显示成"等人点头 / 执行中"
    if (b.kind === 'run_finished') {
      for (const slot of this.gates.values()) {
        if ((slot.node.blocks[slot.index] as GateBlock).approved === null) {
          this.patch<GateBlock>(slot, { ended: true });
        }
      }
      for (const slot of this.tools.values()) {
        if ((slot.node.blocks[slot.index] as ToolBlock).ok === null) {
          this.patch<ToolBlock>(slot, { ended: true });
        }
      }
      // 不收口的话这一步一直转圈，耗时一直往上涨
      for (const node of this.nodes.values()) {
        if (!isOpen(node.status)) continue;
        node.status = 'interrupted';
        if (node.startedAt) node.finishedAt = event.ts;
        this.dirty.add(node.key);
      }
      return;
    }

    // 其余 run 级事件（入队 / 开始）在页头已经写着了，这里只讲节点里发生的事
    if (!event.node_key) return;
    const node = this.nodeOf(event.node_key);
    this.dirty.add(node.key);

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
        this.append(node, { kind: 'note', seq, level: 'info', text: `跳过：${b.reason}` });
        break;
      case 'node_retrying':
        this.append(node, {
          kind: 'retry',
          seq,
          attempt: b.attempt,
          delayMs: b.delay_ms,
          reason: b.reason,
          fedBack: b.feeding_error_to_model
        });
        break;

      case 'agent_invoked':
        this.append(node, { kind: 'command', seq, command: b.command, ranOn: b.ran_on ?? null });
        break;
      case 'agent_turn_started':
        this.append(node, { kind: 'turn', seq, turn: b.turn });
        break;
      case 'agent_thinking':
        this.append(node, { kind: 'thinking', seq, text: b.text });
        break;
      case 'agent_text':
        this.append(node, { kind: 'say', seq, text: b.text });
        break;

      case 'tool_requested':
        this.tools.set(b.tool_use_id, {
          node,
          index:
            node.blocks.push({
              kind: 'tool',
              seq,
              id: b.tool_use_id,
              tool: b.tool,
              input: b.input,
              effect: null,
              reason: null,
              ok: null,
              preview: null,
              durationMs: null,
              ended: false
            }) - 1
        });
        break;
      case 'policy_decided': {
        const slot = this.tools.get(b.tool_use_id);
        if (slot) {
          this.patch<ToolBlock>(slot, { effect: b.effect as PolicyEffect, reason: b.reason });
        } else {
          // 配不上请求的判决说明事件流不完整。让它可见，别静默丢掉
          this.append(node, {
            kind: 'note',
            seq,
            level: b.effect === 'deny' ? 'warn' : 'info',
            text: `策略 ${b.effect}：${b.reason}`
          });
        }
        break;
      }
      case 'tool_completed': {
        const slot = this.tools.get(b.tool_use_id);
        if (slot) {
          this.patch<ToolBlock>(slot, {
            ok: b.ok,
            preview: b.output_preview,
            durationMs: Number(b.duration_ms)
          });
        } else {
          this.append(node, {
            kind: 'note',
            seq,
            level: b.ok ? 'info' : 'warn',
            text: `工具结束：${b.output_preview}`
          });
        }
        break;
      }

      case 'approval_requested':
        this.gates.set(b.approval_id, {
          node,
          index:
            node.blocks.push({
              kind: 'gate',
              seq,
              approvalId: b.approval_id,
              title: b.title,
              approved: null,
              by: null,
              reason: null,
              ended: false
            }) - 1
        });
        break;
      case 'approval_decided': {
        // 走到这里说明 id 对不上请求（事件流不完整）：退回到"这一步里最近一张没结论的卡"
        for (let at = node.blocks.length - 1; at >= 0; at--) {
          const gate = node.blocks[at];
          if (gate.kind === 'gate' && gate.approved === null) {
            this.patch<GateBlock>(
              { node, index: at },
              { approved: b.approved, by: b.decided_by ?? null, reason: b.reason ?? null }
            );
            break;
          }
        }
        break;
      }

      case 'usage':
        node.costMicros += toMicros(String(b.cost_usd));
        this.append(node, {
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
        this.append(node, {
          kind: 'note',
          seq,
          level: 'warn',
          // 上限没被强制这件事必须说出来，不然人会以为 limits 生效了。
          // 原因是目标机探测的原文，很长，每一步都会重复一遍，收进说明里
          text: `资源上限没生效，只能按 ${b.mode} 记账`,
          ...(b.detail ? { detail: b.detail } : {})
        });
        break;
      case 'drift_detected':
        this.append(node, {
          kind: 'note',
          seq,
          level: 'warn',
          text: `行为漂移：输入条件没变，${b.changed_paths.join('、')} 变了`
        });
        break;
      case 'map_expanded':
        this.append(node, {
          kind: 'note',
          seq,
          level: 'info',
          text: `展开 ${b.count} 个实例（上游共 ${b.available} 项）`
        });
        break;
      case 'log':
        this.append(node, { kind: 'note', seq, level: b.level, text: b.message });
        break;
      default:
        break;
    }
  }

  /**
   * 当前的执行过程。没变的节点返回上一次的同一个对象；变了的节点给新对象，
   * 但里面没变的块仍然是原来那个对象。
   */
  snapshot(): NodeRun[] {
    for (const key of this.dirty) {
      const node = this.nodes.get(key);
      if (node) this.snapshots.set(key, { ...node, blocks: node.blocks.slice() });
    }
    this.dirty.clear();
    return this.order.map((key) => this.snapshots.get(key) as NodeRun);
  }

  private nodeOf(key: string): NodeRun {
    let node = this.nodes.get(key);
    if (!node) {
      node = {
        key,
        name: this.names[key] ?? key,
        status: null,
        startedAt: null,
        finishedAt: null,
        attempt: 1,
        error: null,
        output: null,
        costMicros: 0,
        blocks: []
      };
      this.nodes.set(key, node);
      this.order.push(key);
    }
    return node;
  }

  /** 追加一块；和上一块同类的增量文本就拼进上一块（换成新对象）。 */
  private append(node: NodeRun, block: Block): void {
    const at = node.blocks.length - 1;
    const last = node.blocks[at];
    // 连续的增量拼回整段话，不然一段回答会被拆成十几个孤立的块
    if (last && last.kind === block.kind && MERGEABLE.has(block.kind)) {
      node.blocks[at] = {
        ...last,
        text: (last as { text: string }).text + (block as { text: string }).text
      } as Block;
      return;
    }
    node.blocks.push(block);
  }

  /** 改一块：换成一个新对象，原来那个不动（界面按引用判断要不要重画）。 */
  private patch<T extends Block>(slot: Slot, changes: Partial<T>): void {
    slot.node.blocks[slot.index] = { ...(slot.node.blocks[slot.index] as T), ...changes };
    this.dirty.add(slot.node.key);
  }
}

/**
 * 事件流 → 按节点分好组的执行过程（一次性全量）。增量场景用 [`ProcessBuilder`]。
 */
export function groupProcess(events: RunEvent[], names: Record<string, string>): NodeRun[] {
  const builder = new ProcessBuilder(names);
  for (const event of events) builder.push(event);
  return builder.snapshot();
}

/** 编排 → 节点 key 到展示名。画布上叫什么，过程里就叫什么。 */
export function nodeNames(spec: { nodes: Array<{ key: string; name?: string | null }> } | null) {
  const out: Record<string, string> = {};
  for (const node of spec?.nodes ?? []) out[node.key] = node.name || node.key;
  return out;
}

/**
 * 工具调用收起时那一行摘要：最能说明"这次调用在干什么"的那个参数。
 * 认不出的工具退回参数的 JSON，截短。
 */
export function toolSummary(tool: string, input: unknown): string {
  const args = (input ?? {}) as Record<string, unknown>;
  const pick = (...keys: string[]) => {
    for (const key of keys) {
      const value = args[key];
      if (typeof value === 'string' && value.trim()) return value;
    }
    return null;
  };
  // MCP 工具报上来是 `mcp__<server>__<tool>`，按最后一段认
  const name = tool.includes('__') ? tool.slice(tool.lastIndexOf('__') + 2) : tool;
  const hit = /bash$/i.test(name)
    ? pick('command')
    : /^(Read|Write|Edit|MultiEdit|NotebookEdit)$|^remote_(read|write)$/.test(name)
      ? pick('file_path', 'path')
      : /^(Glob|Grep)$|^remote_(glob|grep)$/.test(name)
        ? pick('pattern', 'path')
        : /^Web(Fetch|Search)$/.test(name)
          ? pick('url', 'query')
          : null;
  const text = hit ?? JSON.stringify(input) ?? '';
  const flat = text.replace(/\s+/g, ' ').trim();
  return flat.length > 160 ? `${flat.slice(0, 160)}…` : flat;
}

/**
 * 整次 run 的汇总，同样按事件增量累积：页头的状态和花费、资源图的刷新时机、
 * 原始事件的类型计数，都不用再把全部事件扫一遍。
 */
export class RunTally {
  /** 由事件推出来的状态。`null` = 还没见到 run_started / run_finished。 */
  status: string | null = null;
  startedAt: string | null = null;
  finishedAt: string | null = null;
  /** 模型花费，整数微美元。执行中由 usage 累加，结束时以 run_finished 为准。 */
  costMicros = 0;
  /** 跑完的节点数。资源曲线只有节点跑完才有新采样可读，拿它当刷新信号。 */
  finishedNodes = 0;
  /** 只能降级采样的节点：曲线不完整，资源上限也没生效。 */
  degraded: Record<string, { mode: string; detail: string | null }> = {};
  /** 事件类型 → 条数。原始事件的类型筛选用。 */
  readonly kinds = new Map<string, number>();
  total = 0;

  push(event: RunEvent): void {
    const b = event.body;
    this.total += 1;
    this.kinds.set(b.kind, (this.kinds.get(b.kind) ?? 0) + 1);
    switch (b.kind) {
      case 'run_started':
        this.status = 'running';
        this.startedAt ??= event.ts;
        break;
      case 'run_finished':
        this.status = b.status;
        this.finishedAt = event.ts;
        this.costMicros = toMicros(String(b.cost_usd));
        this.finishedNodes += 1;
        break;
      case 'usage':
        this.costMicros += toMicros(String(b.cost_usd));
        break;
      case 'node_finished':
        this.finishedNodes += 1;
        break;
      case 'resource_degraded':
        if (event.node_key) {
          this.degraded = {
            ...this.degraded,
            [event.node_key]: { mode: b.mode, detail: b.detail ?? null }
          };
        }
        break;
      default:
        break;
    }
  }
}
