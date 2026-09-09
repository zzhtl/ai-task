<script lang="ts">
  /**
   * 每页统一的头：面包屑 + 标题 + 一行摘要 + 右侧动作。
   * 顺手把浏览器标签页的标题也设了：十个 ai-task 标签页并排时得能认出哪个是哪个。
   */
  let {
    title,
    crumb,
    crumbHref,
    sub,
    actions,
    children
  }: {
    title: string;
    crumb?: string;
    crumbHref?: string;
    sub?: import('svelte').Snippet;
    actions?: import('svelte').Snippet;
    /** 标题右侧的小件（状态、徽标）。 */
    children?: import('svelte').Snippet;
  } = $props();
</script>

<svelte:head><title>{title} · ai-task</title></svelte:head>

<header class="page-head">
  <div class="left">
    {#if crumb}
      <a class="crumb" href={crumbHref ?? '/'}>
        <svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6" /></svg>
        {crumb}
      </a>
    {/if}
    <div class="title-row">
      <h1>{title}</h1>
      {#if children}{@render children()}{/if}
    </div>
    {#if sub}<div class="sub">{@render sub()}</div>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>

<style>
  .page-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--s4);
    margin-bottom: var(--s5);
    padding-bottom: var(--s4);
    border-bottom: 1px solid var(--line);
  }
  .left {
    min-width: 0;
  }
  .crumb {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    font-size: 0.78rem;
    color: var(--fg-faint);
    margin-bottom: 4px;
    margin-left: -2px;
  }
  .crumb svg {
    width: 13px;
    height: 13px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .crumb:hover {
    color: var(--accent-fg);
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: var(--s3);
    flex-wrap: wrap;
  }
  h1 {
    overflow-wrap: anywhere;
  }
  .sub {
    margin-top: var(--s2);
    font-size: 0.84rem;
    color: var(--fg-dim);
    display: flex;
    align-items: center;
    gap: var(--s3);
    flex-wrap: wrap;
    line-height: 1.6;
  }
  .actions {
    display: flex;
    gap: var(--s2);
    flex-shrink: 0;
    align-items: center;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  @media (max-width: 700px) {
    .page-head {
      flex-direction: column;
    }
    .actions {
      justify-content: flex-start;
    }
  }
</style>
