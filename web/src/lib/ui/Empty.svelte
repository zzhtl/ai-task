<script lang="ts">
  /** 空状态。不是一句「暂无数据」——要说清为什么空、下一步做什么。 */
  let {
    title,
    hint,
    action
  }: { title: string; hint?: string; action?: import('svelte').Snippet } = $props();
</script>

<div class="empty">
  <strong>{title}</strong>
  {#if hint}<span>{hint}</span>{/if}
  {#if action}{@render action()}{/if}
</div>

<style>
  strong { color: var(--fg); font-weight: 500; }
  span { font-size: 0.85rem; }
</style>
