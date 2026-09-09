<script lang="ts">
  /** 每页统一的头：标题 + 面包屑 + 右侧动作。取代原来各页自己写的「← 返回」。 */
  let {
    title,
    crumb,
    crumbHref,
    sub,
    actions
  }: {
    title: string;
    crumb?: string;
    crumbHref?: string;
    sub?: import('svelte').Snippet;
    actions?: import('svelte').Snippet;
  } = $props();
</script>

<header>
  <div class="left">
    {#if crumb}
      <a class="crumb" href={crumbHref ?? '/'}>{crumb}</a>
    {/if}
    <h1>{title}</h1>
    {#if sub}<div class="sub">{@render sub()}</div>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>

<style>
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--s4);
    margin-bottom: var(--s5);
  }
  .crumb {
    display: block;
    font-size: 0.78rem;
    color: var(--fg-faint);
    margin-bottom: 2px;
  }
  .crumb:hover { color: var(--accent-fg); }
  .sub {
    margin-top: var(--s1);
    font-size: 0.85rem;
    color: var(--fg-dim);
    display: flex;
    align-items: center;
    gap: var(--s3);
    flex-wrap: wrap;
  }
  .actions { display: flex; gap: var(--s2); flex-shrink: 0; }
</style>
