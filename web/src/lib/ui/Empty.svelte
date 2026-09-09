<script lang="ts">
  /** 空状态。不是一句「暂无数据」——要说清为什么空、下一步做什么。 */
  let {
    title,
    hint,
    compact = false,
    action
  }: { title: string; hint?: string; compact?: boolean; action?: import('svelte').Snippet } =
    $props();
</script>

<div class="empty" class:compact>
  <span class="glyph" aria-hidden="true"></span>
  <strong>{title}</strong>
  {#if hint}<span class="hint">{hint}</span>{/if}
  {#if action}<div class="act">{@render action()}</div>{/if}
</div>

<style>
  strong {
    color: var(--fg);
    font-weight: 500;
  }
  .hint {
    font-size: 0.84rem;
    max-width: 44ch;
    line-height: 1.6;
  }
  .glyph {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    border: 1px dashed var(--line-strong);
    position: relative;
    margin-bottom: var(--s1);
  }
  .glyph::after {
    content: '';
    position: absolute;
    inset: 9px;
    border-radius: 50%;
    background: var(--line-strong);
  }
  .act {
    margin-top: var(--s2);
    display: flex;
    gap: var(--s2);
  }
  .compact {
    padding: var(--s4);
    gap: var(--s1);
  }
  .compact .glyph {
    display: none;
  }
</style>
