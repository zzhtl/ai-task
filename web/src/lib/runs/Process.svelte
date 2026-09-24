<script lang="ts">
  /**
   * 执行过程：一次 run 里每一步到底做了什么。
   *
   * 一步一段，段里按时间顺序讲：说了什么、调了什么工具（收成一行，点开看参数和返回）、
   * 等了谁的审批、花了多少钱。思考默认收起——那是模型的自言自语，要看再点开。
   *
   * 数据来自 `ProcessBuilder` 的快照：没变的节点和块是同一个对象，
   * 按 key / seq 渲染的这两层只会重画变了的那一段。
   */
  import { isOpen, type NodeRun } from './process';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import StepRail from '$lib/ui/StepRail.svelte';
  import CopyButton from '$lib/ui/CopyButton.svelte';
  import Fold from '$lib/ui/Fold.svelte';
  import HelpTip from '$lib/ui/HelpTip.svelte';
  import ToolCall from './ToolCall.svelte';
  import { duration, moneyMicros, toMicros } from '$lib/ui/format';

  let {
    nodes,
    pending = [],
    hosts = [],
    position = {},
    waiting = new Set()
  }: {
    nodes: NodeRun[];
    /** 编排里有、但还没跑到的步骤。审批节点在等人时事件还没写，也在这里，状态是等待审批。 */
    pending?: Array<{ key: string; name: string; waiting?: boolean }>;
    hosts?: Array<{ id: string; name: string }>;
    /** 节点 key → 编排里的序号（从 1 开始）。没有就按出现顺序编号。 */
    position?: Record<string, number>;
    /** 有待审批的节点 key。已经开始、还没结论的审批门显示成等待审批。 */
    waiting?: Set<string>;
  } = $props();

  const hostLabel = (id: string | null) =>
    id ? (hosts.find((h) => h.id === id)?.name ?? id.slice(0, 8)) : '中心';

  const total = $derived(nodes.length + pending.length);
  const awaiting = (node: NodeRun) => isOpen(node.status) && waiting.has(node.key);
  const IDLE: Record<string, string> = {
    running: '已开始，等第一条输出…',
    ready: '准备开始…',
    interrupted: 'run 结束时这一步还没跑完。'
  };

  /**
   * 每一步默认只画最后这么多块。长的 AI 步骤一步就有上千块：全画出来 DOM 上万，
   * 每来一帧新事件都要把整张列表 diff 一遍，主线程一卡就是一百多毫秒。
   * 结论在末尾，更早的按需往上展开。
   */
  const WINDOW = 80;
  const MORE = 200;
  let shown = $state<Record<string, number>>({});
</script>

<section class="process">
  {#each nodes as node, i (node.key)}
    {@const limit = shown[node.key] ?? WINDOW}
    {@const hidden = Math.max(0, node.blocks.length - limit)}
    <!-- data-node 是编排图和步骤导航点过来时滚动的锚点 -->
    <article class="node" data-node={node.key}>
      <StepRail index={position[node.key] ?? i + 1} last={i === total - 1} />

      <div class="body">
        <header class="node-head">
          <span class="name">{node.name}</span>
          {#if node.name !== node.key}<span class="key mono">{node.key}</span>{/if}
          {#if awaiting(node)}
            <StatusBadge status="awaiting_approval" />
          {:else if node.status}
            <StatusBadge status={node.status} />
          {/if}
          {#if node.attempt > 1}<span class="tag warn">第 {node.attempt} 次尝试</span>{/if}
          <span class="spacer"></span>
          {#if node.startedAt}<span class="faint">{duration(node.startedAt, node.finishedAt)}</span>{/if}
          {#if node.costMicros > 0}<span class="faint">{moneyMicros(node.costMicros)}</span>{/if}
        </header>

        {#if awaiting(node)}
          <p class="idle">等人批准或拒绝，审批卡在页面顶部。</p>
        {:else if node.blocks.length === 0 && !node.error && node.output == null}
          <p class="idle">{IDLE[node.status ?? 'ready'] ?? '没有输出。'}</p>
        {/if}

        {#if hidden > 0}
          <button type="button" class="btn-ghost btn-sm earlier" onclick={() => (shown[node.key] = limit + MORE)}>
            显示更早的 {Math.min(hidden, MORE)} 条（共 {hidden} 条没显示）
          </button>
        {/if}
        {#each hidden > 0 ? node.blocks.slice(hidden) : node.blocks as block (block.seq)}
          {#if block.kind === 'command'}
            <Fold label="执行命令" hint="在{hostLabel(block.ranOn)}">
              <div class="cmd">
                <span class="copy"><CopyButton text={block.command} label="复制命令" /></span>
                <pre class="mono">{block.command}</pre>
              </div>
            </Fold>
          {:else if block.kind === 'turn'}
            <div class="turn"><span>第 {block.turn} 轮</span></div>
          {:else if block.kind === 'thinking'}
            <Fold label="思考" hint="{block.text.length} 字" tone="subtle">
              <p class="thinking">{block.text}</p>
            </Fold>
          {:else if block.kind === 'say'}
            <p class="say">{block.text}</p>
          {:else if block.kind === 'tool'}
            <ToolCall {block} />
          {:else if block.kind === 'gate'}
            <div class="gate">
              <span class="lbl">审批</span>
              <span class="gate-title">{block.title}</span>
              {#if block.approved === null}
                <span class="tag" class:warn={!block.ended}>{block.ended ? '没有结论（run 已结束）' : '等人点头'}</span>
              {:else if block.approved}
                <span class="tag ok">已通过{block.by ? `（${block.by}）` : ''}</span>
              {:else}
                <!-- 决策人为空不代表超时：没开认证时人工拒绝也没有名字。是不是超时看原因 -->
                <span class="tag danger">已拒绝{block.by ? `（${block.by}）` : ''}</span>
              {/if}
              {#if block.reason}<span class="gate-reason">{block.reason}</span>{/if}
            </div>
          {:else if block.kind === 'retry'}
            <p class="retry">
              第 {block.attempt} 次失败：{block.reason}
              <span class="faint">{block.delayMs}ms 后重试{block.fedBack ? '，并把错误回喂给模型' : ''}</span>
            </p>
          {:else if block.kind === 'usage'}
            <p class="usage">
              <span class="mono">{block.model}</span>
              <span>入 {block.inTok} / 出 {block.outTok}</span>
              {#if block.cacheRead}<span>命中缓存 {block.cacheRead}</span>{/if}
              <span>{moneyMicros(toMicros(block.costUsd))}</span>
            </p>
          {:else if block.kind === 'note'}
            <p class="note" class:warn={block.level === 'warn' || block.level === 'error'}>
              {block.text}
              {#if block.detail}<HelpTip text={block.detail} label="原因" />{/if}
            </p>
          {/if}
        {/each}

        {#if node.error}
          <p class="node-error">{node.error}</p>
        {/if}
        {#if node.output !== null && node.output !== undefined}
          <Fold label="这一步的结果">
            <div class="cmd">
              <span class="copy"><CopyButton text={JSON.stringify(node.output, null, 2)} label="复制结果" /></span>
              <pre class="mono">{JSON.stringify(node.output, null, 2)}</pre>
            </div>
          </Fold>
        {/if}
      </div>
    </article>
  {/each}

  {#each pending as step, i (step.key)}
    <article class="node pending" data-node={step.key}>
      <StepRail index={position[step.key] ?? nodes.length + i + 1} last={nodes.length + i === total - 1} />
      <div class="body compact">
        <header class="node-head">
          <span class="name">{step.name}</span>
          {#if step.waiting}
            <StatusBadge status="awaiting_approval" />
          {:else}
            <StatusBadge status="pending" label="还没开始" />
          {/if}
        </header>
      </div>
    </article>
  {/each}
</section>

<style>
  .process {
    display: flex;
    flex-direction: column;
  }
  .node {
    display: grid;
    grid-template-columns: 2rem minmax(0, 1fr);
    gap: var(--s3);
  }
  .body {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    box-shadow: var(--shadow-card);
    padding: var(--s3) var(--s4);
    margin-bottom: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
  }
  .body.compact {
    padding: var(--s2) var(--s4);
    box-shadow: none;
    background: transparent;
    border-style: dashed;
  }
  .node-head {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
    min-height: 1.75rem;
  }
  .name {
    font-weight: 600;
  }
  .key {
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .faint {
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .earlier {
    align-self: flex-start;
  }
  .idle {
    margin: 0;
    font-size: var(--t-sm);
    color: var(--fg-faint);
  }

  /* 模型说的话：正文，给人读的结论，正常亮度 */
  .say {
    margin: 0;
    padding-left: var(--s3);
    border-left: 2px solid var(--accent-border);
    font-size: var(--t-base);
    line-height: 1.75;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--fg);
  }
  .thinking {
    margin: 0;
    padding-left: var(--s3);
    border-left: 2px solid var(--line-strong);
    font-size: var(--t-sm);
    line-height: 1.7;
    color: var(--fg-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  /* 轮次只是个分隔，不该抢戏 */
  .turn {
    display: flex;
    align-items: center;
    gap: var(--s2);
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .turn::after {
    content: '';
    flex: 1;
    height: 1px;
    background: var(--line);
  }

  .gate {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
    padding: var(--s2) var(--s3);
    border: 1px solid var(--warn-border);
    border-radius: var(--r2);
    background: var(--warn-bg);
    font-size: var(--t-sm);
  }
  .gate .lbl {
    font-size: var(--t-xs);
    color: var(--warn-fg);
    font-weight: 600;
  }
  .gate-title {
    font-weight: 500;
  }
  .gate-reason {
    color: var(--fg-dim);
  }
  .retry {
    margin: 0;
    font-size: var(--t-sm);
    color: var(--warn-fg);
  }
  .usage {
    margin: 0;
    display: flex;
    gap: var(--s2);
    flex-wrap: wrap;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .note {
    margin: 0;
    display: flex;
    align-items: center;
    gap: var(--s1);
    font-size: var(--t-sm);
    color: var(--fg-faint);
  }
  .note.warn {
    color: var(--warn-fg);
  }
  .node-error {
    margin: 0;
    padding: var(--s2) var(--s3);
    border-radius: var(--r2);
    background: var(--bad-bg);
    color: var(--bad-fg);
    font-size: var(--t-sm);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .cmd {
    position: relative;
  }
  .cmd .copy {
    position: absolute;
    top: var(--s1);
    right: var(--s1);
  }
  .cmd pre {
    margin: 0;
    padding: var(--s3);
    padding-right: 2.5rem;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r2);
    font-size: var(--t-xs);
    line-height: 1.65;
    color: var(--fg-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 20rem;
    overflow-y: auto;
  }
</style>
