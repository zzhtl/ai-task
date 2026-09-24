<script lang="ts">
  /**
   * 一次工具调用：请求、策略判决、结果合成一张卡。
   *
   * **默认收成一行**：一次执行里几十次 Read/Grep，全部摊开的话真正要看的
   * 结论被挤到好几屏之外。被策略拦下的、失败的默认展开——那正是出问题时要看的。
   */
  import type { ToolBlock } from './process';
  import { toolSummary } from './process';
  import Icon from '$lib/ui/Icon.svelte';

  let { block }: { block: ToolBlock } = $props();

  const denied = $derived(block.effect === 'deny');
  const failed = $derived(block.ok === false);
  // 只在第一次渲染时决定默认展开与否；之后听人的
  // svelte-ignore state_referenced_locally
  let open = $state(block.effect === 'deny' || block.ok === false);
  // 后来才被拒绝 / 失败的（执行中收到判决）也要自动展开一次
  let autoOpened = $state(false);
  $effect(() => {
    if ((denied || failed) && !autoOpened) {
      autoOpened = true;
      open = true;
    }
  });

  const running = $derived(block.ok === null && !denied && !block.ended);
  const summary = $derived(toolSummary(block.tool, block.input));
  const pretty = $derived(open ? JSON.stringify(block.input, null, 2) : '');
</script>

<div class="tool" class:denied class:failed class:open>
  <button class="head" type="button" aria-expanded={open} onclick={() => (open = !open)}>
    <span class="state" class:spin={running}>
      {#if denied}
        <Icon name="ban" />
      {:else if failed}
        <Icon name="close" />
      {:else if running}
        <Icon name="loader" />
      {:else if block.ok}
        <Icon name="check" />
      {:else}
        <Icon name="circle" />
      {/if}
    </span>
    <b class="name">{block.tool}</b>
    <span class="summary mono">{summary}</span>
    <span class="spacer"></span>
    {#if block.effect === 'ask'}<span class="tag warn">需审批</span>{/if}
    {#if denied}<span class="tag danger">被策略拒绝</span>{/if}
    {#if block.durationMs !== null}<span class="dur">{(block.durationMs / 1000).toFixed(1)}s</span>{/if}
    {#if block.ended && block.ok === null && !denied}<span class="dur">没有结果</span>{/if}
    <span class="chev"><Icon name="chevron-down" /></span>
  </button>
  {#if denied && block.reason}
    <!-- 被拦下来的调用要说清楚是哪条规则拦的：这是审计链路的一环 -->
    <p class="why">{block.reason}</p>
  {/if}
  {#if open}
    <div class="body">
      <span class="lbl">输入</span>
      <pre class="mono">{pretty}</pre>
      {#if block.preview !== null}
        <span class="lbl">{failed ? '错误输出' : '输出'}</span>
        <pre class="mono out" class:bad-out={failed}>{block.preview}</pre>
      {/if}
      {#if block.effect && !denied && block.reason}
        <span class="lbl">策略</span>
        <p class="policy">{block.effect} · {block.reason}</p>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tool {
    border: 1px solid var(--line);
    border-radius: var(--r2);
    background: var(--surface-2);
    min-width: 0;
  }
  .tool.denied {
    border-color: var(--bad-border);
    background: var(--bad-bg);
  }
  .tool.failed {
    border-color: var(--warn-border);
  }
  .head,
  :global(:root[data-theme='light']) .head {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    height: auto;
    min-height: 2.125rem;
    padding: 0.3rem var(--s3);
    border: none;
    border-radius: var(--r2);
    background: transparent;
    box-shadow: none;
    color: var(--fg);
    font-size: var(--t-sm);
    font-weight: 400;
    text-align: left;
  }
  .head:hover:not(:disabled) {
    background: var(--surface-3);
  }
  .state {
    display: inline-flex;
    color: var(--fg-faint);
  }
  .tool:not(.denied):not(.failed) .state {
    color: var(--ok-fg);
  }
  .denied .state {
    color: var(--bad-fg);
  }
  .failed .state {
    color: var(--warn-fg);
  }
  .state.spin {
    color: var(--info-fg);
  }
  .state :global(svg) {
    width: 13px;
    height: 13px;
    stroke-width: 2.2;
  }
  .state.spin :global(svg) {
    animation: spin 1.1s linear infinite;
  }
  .name {
    font-weight: 600;
    white-space: nowrap;
  }
  .summary {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--fg-dim);
    font-size: var(--t-xs);
  }
  .dur {
    color: var(--fg-faint);
    font-size: var(--t-xs);
    white-space: nowrap;
  }
  .chev {
    display: inline-flex;
    color: var(--fg-faint);
    transition: transform var(--dur-2) var(--ease);
  }
  .chev :global(svg) {
    width: 13px;
    height: 13px;
  }
  .open .chev {
    transform: rotate(180deg);
  }
  .why {
    margin: 0;
    padding: 0 var(--s3) var(--s2) 2.1rem;
    font-size: var(--t-sm);
    color: var(--bad-fg);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding: 0 var(--s3) var(--s3);
  }
  .lbl {
    font-size: var(--t-xs);
    color: var(--fg-faint);
    margin-top: var(--s1);
  }
  pre {
    margin: 0;
    padding: var(--s2) var(--s3);
    border: 1px solid var(--line);
    border-radius: var(--r2);
    background: var(--surface-1);
    font-size: var(--t-xs);
    line-height: 1.6;
    color: var(--fg-dim);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 18rem;
    overflow-y: auto;
  }
  .bad-out {
    border-color: var(--warn-border);
  }
  .policy {
    margin: 0;
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
</style>
