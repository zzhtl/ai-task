<script lang="ts">
  /**
   * 执行过程：一次 run 里 AI 到底做了什么。
   *
   * 一步一段，段里按时间顺序讲：想了什么、调了什么工具、工具返回了什么、
   * 说了什么、花了多少钱。**工具调用的请求、策略判决、返回结果合成一张卡**
   * ——它们本来就是同一件事，拆成三行读的人得自己在脑子里拼。
   *
   * 原始事件流没有被取代，它挪到下面折起来了：那是排查用的，这里是给人读的。
   */
  import type { RunEvent } from '$api/types/RunEvent';
  import type { DagSpec } from '$api/types/DagSpec';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import { duration, money } from '$lib/ui/format';
  import { groupProcess, nodeNames } from './process';

  let {
    events,
    spec = null,
    hosts = []
  }: {
    events: RunEvent[];
    spec?: DagSpec | null;
    hosts?: Array<{ id: string; name: string }>;
  } = $props();

  const nodes = $derived(groupProcess(events, nodeNames(spec as never)));

  const hostLabel = (id: string | null) =>
    id ? (hosts.find((h) => h.id === id)?.name ?? id.slice(0, 8)) : '中心';

  const brief = (input: unknown) => {
    const s = JSON.stringify(input) ?? '';
    return s.length > 200 ? `${s.slice(0, 200)}…` : s;
  };

  let copied = $state<number | null>(null);
  async function copy(seq: number, text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = seq;
      setTimeout(() => (copied = null), 1500);
    } catch {
      copied = null;
    }
  }
</script>

{#if nodes.length}
  <section class="process">
    <header class="head">
      <h2>执行过程</h2>
      <span class="faint">每一步想了什么、调了什么、返回了什么</span>
    </header>

    {#each nodes as node, i (node.key)}
      <article class="node">
        <div class="rail">
          <span class="num">{i + 1}</span>
          {#if i < nodes.length - 1}<span class="wire"></span>{/if}
        </div>

        <div class="body">
          <header class="node-head">
            <span class="name">{node.name}</span>
            <span class="key mono">{node.key}</span>
            {#if node.status}<StatusPill status={node.status} />{/if}
            {#if node.attempt > 1}<span class="tag">第 {node.attempt} 次尝试</span>{/if}
            <span class="spacer"></span>
            {#if node.startedAt}
              <span class="faint">{duration(node.startedAt, node.finishedAt)}</span>
            {/if}
            {#if node.costUsd > 0}<span class="faint">{money(node.costUsd)}</span>{/if}
          </header>

          {#if node.blocks.length === 0}
            <p class="faint idle">还没开始。</p>
          {/if}

          {#each node.blocks as block (block.seq)}
            {#if block.kind === 'command'}
              <details class="cmd">
                <summary>
                  执行命令 <span class="tag" class:accent={block.ranOn}>在{hostLabel(block.ranOn)}</span>
                </summary>
                <div class="cmd-body">
                  <button class="btn-ghost btn-sm copy" onclick={() => copy(block.seq, block.command)}>
                    {copied === block.seq ? '已复制' : '复制'}
                  </button>
                  <pre class="mono">{block.command}</pre>
                </div>
              </details>
            {:else if block.kind === 'turn'}
              <div class="turn"><span>第 {block.turn} 轮</span></div>
            {:else if block.kind === 'thinking'}
              <div class="blk thinking">
                <span class="lbl">思考</span>
                <p>{block.text}</p>
              </div>
            {:else if block.kind === 'say'}
              <div class="blk say">
                <span class="lbl">输出</span>
                <p>{block.text}</p>
              </div>
            {:else if block.kind === 'tool'}
              <div class="blk tool" class:denied={block.effect === 'deny'} class:failed={block.ok === false}>
                <div class="tool-head">
                  <span class="lbl">工具</span>
                  <b>{block.tool}</b>
                  {#if block.effect}
                    <span class="tag" class:danger={block.effect === 'deny'}>策略 {block.effect}</span>
                  {/if}
                  <span class="spacer"></span>
                  {#if block.durationMs !== null}<span class="faint">{(block.durationMs / 1000).toFixed(1)}s</span>{/if}
                  {#if block.ok === null && block.effect !== 'deny'}<span class="faint">执行中…</span>{/if}
                </div>
                <pre class="mono arg">{brief(block.input)}</pre>
                {#if block.effect === 'deny'}
                  <!-- 被拦下来的调用要说清楚是哪条规则拦的：这是审计链路的一环 -->
                  <p class="denied-why">被策略拦下：{block.reason}</p>
                {:else if block.preview !== null}
                  <pre class="mono out" class:bad-out={block.ok === false}>{block.preview}</pre>
                {/if}
              </div>
            {:else if block.kind === 'gate'}
              <div class="blk gate">
                <span class="lbl">审批</span>
                <p>
                  {block.title}
                  {#if block.approved === null}
                    <span class="tag">等人点头</span>
                  {:else if block.approved}
                    <span class="tag ok">已通过{block.by ? `（${block.by}）` : ''}</span>
                  {:else}
                    <span class="tag danger">已拒绝{block.by ? `（${block.by}）` : '（超时）'}</span>
                  {/if}
                  {#if block.reason}<em>{block.reason}</em>{/if}
                </p>
              </div>
            {:else if block.kind === 'retry'}
              <div class="blk retry">
                <span class="lbl">重试</span>
                <p>
                  第 {block.attempt} 次失败：{block.reason}
                  <span class="faint">
                    {block.delayMs}ms 后再来{block.fedBack ? '，并把错误回喂给模型' : ''}
                  </span>
                </p>
              </div>
            {:else if block.kind === 'usage'}
              <p class="usage faint">
                <span class="mono">{block.model}</span>
                入 {block.inTok} / 出 {block.outTok}
                {#if block.cacheRead}· 命中缓存 {block.cacheRead}{/if}
                · {money(block.costUsd)}
              </p>
            {:else if block.kind === 'note'}
              <p class="note" class:warn={block.level === 'warn' || block.level === 'error'}>
                {block.text}
              </p>
            {/if}
          {/each}

          {#if node.error}
            <p class="bad err">{node.error}</p>
          {/if}
          {#if node.output !== null && node.output !== undefined}
            <details class="out-detail">
              <summary class="faint">这一步的结果</summary>
              <pre class="mono">{JSON.stringify(node.output, null, 2)}</pre>
            </details>
          {/if}
        </div>
      </article>
    {/each}
  </section>
{/if}

<style>
  .process {
    margin-top: var(--s4);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    flex-wrap: wrap;
    margin-bottom: var(--s3);
  }
  .head h2 {
    margin: 0;
  }
  .faint {
    font-size: 0.76rem;
    color: var(--fg-faint);
  }

  .node {
    display: grid;
    grid-template-columns: 2rem 1fr;
    gap: var(--s3);
  }
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s1);
  }
  .num {
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 50%;
    border: 1px solid var(--line-strong);
    background: var(--surface-2);
    display: grid;
    place-items: center;
    font-size: 0.78rem;
    color: var(--fg-dim);
    flex: 0 0 auto;
  }
  .wire {
    flex: 1;
    width: 1px;
    background: var(--line);
    min-height: var(--s4);
  }
  .body {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s3);
    margin-bottom: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
  }
  .node-head {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .name {
    font-weight: 500;
  }
  .key {
    font-size: 0.72rem;
    color: var(--fg-faint);
  }
  .idle {
    margin: 0;
  }

  /* 一块 = 一件事。左边一条竖线把它和上一件事分开，比卡片边框轻 */
  .blk {
    display: grid;
    grid-template-columns: 3rem 1fr;
    gap: var(--s2);
    padding-left: var(--s2);
    border-left: 2px solid var(--line);
    min-width: 0;
  }
  .blk p {
    margin: 0;
    font-size: 0.86rem;
    line-height: 1.7;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .lbl {
    font-size: 0.72rem;
    color: var(--fg-faint);
    padding-top: 0.2rem;
  }

  /* 思考是模型的自言自语，压暗；输出是给人看的结论，正常亮度 */
  .thinking {
    border-left-color: color-mix(in srgb, var(--line-strong) 60%, transparent);
  }
  .thinking p {
    color: var(--fg-faint);
    font-size: 0.82rem;
  }
  .say {
    border-left-color: var(--accent-dim);
  }
  .say p {
    color: var(--fg);
  }

  .tool {
    grid-template-columns: 1fr;
    gap: var(--s1);
  }
  .tool.denied {
    border-left-color: var(--bad);
  }
  .tool.failed {
    border-left-color: var(--warn);
  }
  .tool-head {
    display: flex;
    align-items: center;
    gap: var(--s2);
    font-size: 0.84rem;
    flex-wrap: wrap;
  }
  .arg,
  .out {
    margin: 0;
    padding: var(--s2);
    background: var(--surface-2);
    border-radius: var(--r2);
    font-size: 0.76rem;
    line-height: 1.6;
    color: var(--fg-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 14rem;
    overflow-y: auto;
  }
  .out {
    background: transparent;
    border: 1px solid var(--line);
  }
  .bad-out {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--line));
  }
  .denied-why {
    margin: 0;
    font-size: 0.8rem;
    color: var(--bad);
  }

  .gate {
    border-left-color: var(--warn);
  }
  .gate em {
    font-style: normal;
    color: var(--fg-faint);
  }
  .retry {
    border-left-color: var(--warn);
  }

  /* 轮次只是个分隔，不该抢戏 */
  .turn {
    display: flex;
    align-items: center;
    gap: var(--s2);
    font-size: 0.72rem;
    color: var(--fg-faint);
  }
  .turn::after {
    content: '';
    flex: 1;
    height: 1px;
    background: var(--line);
  }

  .usage {
    margin: 0;
    display: flex;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .note {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-faint);
  }
  .note.warn {
    color: var(--warn);
  }
  .err {
    margin: 0;
  }

  .cmd summary,
  .out-detail summary {
    cursor: pointer;
    font-size: 0.78rem;
    color: var(--fg-faint);
  }
  .cmd summary::before,
  .out-detail summary::before {
    content: '▸ ';
  }
  .cmd[open] summary::before,
  .out-detail[open] summary::before {
    content: '▾ ';
  }
  .cmd-body {
    position: relative;
    margin-top: var(--s2);
  }
  .cmd-body .copy {
    position: absolute;
    top: var(--s1);
    right: var(--s1);
  }
  .cmd pre,
  .out-detail pre {
    margin: 0;
    padding: var(--s3);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r2);
    font-size: 0.76rem;
    line-height: 1.65;
    color: var(--fg-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 20rem;
    overflow-y: auto;
  }
</style>
