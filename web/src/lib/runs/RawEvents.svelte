<script lang="ts">
  /**
   * 原始事件流：按 seq 排、不合并、不隐藏。排查时用。
   *
   * 这里的搜索和类型筛选只是"找"，不是"改"——清空就全回来。
   * **只渲染最近的 500 条**（可以往前翻）：一次长执行有上万条事件，
   * 全部挂进 DOM 会让整页都卡；真要找旧的，用搜索比滚动快。
   */
  import type { RunEvent } from '$api/types/RunEvent';
  import { clock, stamp } from '$lib/ui/format';
  import { describeEvent } from './describe';

  let {
    events,
    revision,
    kinds
  }: {
    /** 普通数组（不是响应式的）；`revision` 变了才重新筛。 */
    events: RunEvent[];
    revision: number;
    kinds: Map<string, number>;
  } = $props();

  const WINDOW = 500;
  let find = $state('');
  let kind = $state<string | null>(null);
  let limit = $state(WINDOW);
  let expanded = $state(new Set<number>());

  const matches = $derived.by(() => {
    void revision;
    const q = find.trim().toLowerCase();
    if (!q && !kind) return events;
    return events.filter(
      (e) =>
        (!kind || e.body.kind === kind) &&
        (!q ||
          e.body.kind.includes(q) ||
          (e.node_key ?? '').toLowerCase().includes(q) ||
          describeEvent(e).toLowerCase().includes(q))
    );
  });
  const shown = $derived(matches.slice(Math.max(0, matches.length - limit)));
  const hidden = $derived(matches.length - shown.length);

  const kindList = $derived.by(() => {
    void revision;
    return [...kinds.entries()].sort((a, b) => b[1] - a[1]);
  });

  $effect(() => {
    // 换了筛选条件就回到最近那一窗
    void find;
    void kind;
    limit = WINDOW;
  });

  function toggle(seq: number) {
    const next = new Set(expanded);
    if (next.has(seq)) next.delete(seq);
    else next.add(seq);
    expanded = next;
  }
</script>

<div class="tools">
  <input bind:value={find} placeholder="找 kind、节点或文本" type="search" spellcheck="false" />
  <div class="kinds">
    <button type="button" class="chip" class:on={kind === null} onclick={() => (kind = null)}>
      全部 <span class="n">{events.length}</span>
    </button>
    {#each kindList as [k, n] (k)}
      <button type="button" class="chip" class:on={kind === k} onclick={() => (kind = kind === k ? null : k)}>
        {k} <span class="n">{n}</span>
      </button>
    {/each}
  </div>
</div>

{#if hidden > 0}
  <div class="earlier">
    <button type="button" class="btn-ghost btn-sm" onclick={() => (limit += WINDOW)}>
      显示更早的 {Math.min(WINDOW, hidden)} 条（还有 {hidden} 条）
    </button>
  </div>
{/if}

<ol class="events">
  {#each shown as event (event.seq)}
    <li
      class="{event.body.kind} {event.body.kind === 'policy_decided' ? `effect-${event.body.effect}` : ''}"
      class:open={expanded.has(event.seq)}
    >
      <button type="button" class="row" onclick={() => toggle(event.seq)}>
        <span class="seq mono">{event.seq}</span>
        <time class="mono" title={stamp(event.ts)}>{clock(event.ts)}</time>
        <span class="kind">{event.body.kind}</span>
        <span class="node mono">{event.node_key ?? ''}</span>
        <span class="text">{describeEvent(event)}</span>
      </button>
    </li>
  {:else}
    <li class="none">{find || kind ? '没有匹配的事件' : '还没有事件'}</li>
  {/each}
</ol>

<style>
  .tools {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    margin-bottom: var(--s3);
  }
  .tools input {
    width: min(26rem, 100%);
  }
  .kinds {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s1);
  }
  .chip,
  :global(:root[data-theme='light']) .kinds .chip {
    height: 1.625rem;
    padding: 0 0.55rem;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--surface-1);
    box-shadow: none;
    color: var(--fg-dim);
    font-size: var(--t-xs);
    font-weight: 500;
  }
  .chip.on {
    border-color: var(--accent-border);
    background: var(--accent-bg);
    color: var(--accent-fg);
  }
  .n {
    color: var(--fg-faint);
    margin-left: 0.2rem;
  }
  .earlier {
    display: flex;
    justify-content: center;
    margin-bottom: var(--s2);
  }
  .events {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: var(--r3);
    overflow: hidden;
    background: var(--surface-1);
  }
  .events li + li {
    border-top: 1px solid var(--line);
  }
  .row,
  :global(:root[data-theme='light']) .events .row {
    display: grid;
    grid-template-columns: 3rem 4.5rem 9rem 6rem minmax(0, 1fr);
    gap: var(--s3);
    align-items: baseline;
    width: 100%;
    height: auto;
    padding: 0.35rem var(--s4);
    border: none;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
    color: var(--fg-dim);
    font-size: var(--t-sm);
    font-weight: 400;
    text-align: left;
  }
  .row:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .seq,
  time,
  .kind {
    color: var(--fg-faint);
    font-size: var(--t-xs);
  }
  .seq {
    text-align: right;
  }
  .node {
    color: var(--fg-dim);
    font-size: var(--t-xs);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* 默认一行一条：一条事件一行才是时间轴；点开这一行再看全文 */
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .open .text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .none {
    padding: var(--s5) var(--s4);
    text-align: center;
    color: var(--fg-faint);
  }
  /* 需要被看见的几类事件。其余保持低对比，免得整屏都在喊 */
  .agent_text .text,
  .run_finished .text,
  .node_finished .text {
    color: var(--fg);
  }
  .drift_detected .text,
  .resource_degraded .text {
    color: var(--warn-fg);
  }
  /* 放行的判决不该是红的——审计里 allow 和 deny 一样多，全红就没有信号了 */
  .effect-deny .text,
  .effect-ask .text {
    color: var(--bad-fg);
  }
  .effect-deny {
    background: var(--bad-bg);
  }
  .effect-allow .kind {
    color: var(--ok-fg);
  }
  @media (max-width: 960px) {
    .row,
    :global(:root[data-theme='light']) .events .row {
      grid-template-columns: 2.5rem minmax(0, 1fr);
    }
    time,
    .kind,
    .node {
      display: none;
    }
  }
</style>
