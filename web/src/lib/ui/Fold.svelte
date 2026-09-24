<script lang="ts">
  /**
   * 可折叠的一段。和原生 `<details>` 的区别：**收起时里面根本不渲染**。
   *
   * 执行过程里的思考、命令行、节点输出动辄上万字；`<details>` 收起时内容照样在 DOM 里，
   * 一次长执行攒下几百段，页面就被这些看不见的节点拖慢了。
   */
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    label,
    hint,
    open = $bindable(false),
    tone = 'plain',
    children
  }: {
    label: string;
    /** 标签后面的一小段补充（字数、主机名）。 */
    hint?: string;
    open?: boolean;
    tone?: 'plain' | 'subtle';
    children: Snippet;
  } = $props();
</script>

<div class="fold {tone}" class:open>
  <button type="button" class="toggle" aria-expanded={open} onclick={() => (open = !open)}>
    <span class="chev"><Icon name="chevron-right" /></span>
    <span class="label">{label}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
  </button>
  {#if open}
    <div class="content">{@render children()}</div>
  {/if}
</div>

<style>
  .fold {
    min-width: 0;
  }
  .toggle,
  :global(:root[data-theme='light']) .fold .toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--s1);
    height: auto;
    padding: 0.15rem var(--s1) 0.15rem 0;
    border: none;
    background: transparent;
    box-shadow: none;
    color: var(--fg-faint);
    font-size: var(--t-sm);
    font-weight: 500;
  }
  .toggle:hover:not(:disabled) {
    background: transparent;
    color: var(--fg);
  }
  .chev {
    display: inline-flex;
    transition: transform var(--dur-2) var(--ease);
  }
  .chev :global(svg) {
    width: 13px;
    height: 13px;
  }
  .open .chev {
    transform: rotate(90deg);
  }
  .hint {
    font-weight: 400;
    font-size: var(--t-xs);
  }
  .content {
    margin-top: var(--s1);
  }
</style>
