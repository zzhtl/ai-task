<script lang="ts">
  /**
   * 每页统一的头：面包屑 + 标题（旁边是状态和说明）+ 一行元信息 + 右侧动作 + 可选页签。
   * 顺手把浏览器标签页的标题也设了：十个 ai-task 标签页并排时得能认出哪个是哪个。
   *
   * 解释性的长段落不放在这里：`help` 收进标题旁的"?"，元信息行只放短事实。
   */
  import type { Snippet } from 'svelte';
  import HelpTip from './HelpTip.svelte';

  let {
    title,
    crumbs = [],
    crumb,
    crumbHref,
    help,
    sub,
    actions,
    tabs,
    children
  }: {
    title: string;
    /** 上级页面，从远到近。当前页就是标题，不用再写进来。 */
    crumbs?: Array<{ label: string; href: string }>;
    /** 只有一级上级时的简写。 */
    crumb?: string;
    crumbHref?: string;
    /** 标题旁"?"里的说明。 */
    help?: string;
    /** 元信息行：几段短事实，自动用圆点隔开。 */
    sub?: Snippet;
    actions?: Snippet;
    /** 标题下方的页签。 */
    tabs?: Snippet;
    /** 标题右侧的小件（状态、徽标）。 */
    children?: Snippet;
  } = $props();

  const trail = $derived(crumb ? [{ label: crumb, href: crumbHref ?? '/' }, ...crumbs] : crumbs);
</script>

<svelte:head><title>{title} · ai-task</title></svelte:head>

<header class="page-head" class:has-tabs={!!tabs}>
  {#if trail.length}
    <nav class="crumbs" aria-label="位置">
      {#each trail as c (c.href + c.label)}
        <a href={c.href}>{c.label}</a>
        <span class="sep" aria-hidden="true">/</span>
      {/each}
    </nav>
  {/if}
  <div class="main-row">
    <div class="left">
      <div class="title-row">
        <h1>{title}</h1>
        {#if help}<HelpTip text={help} />{/if}
        {#if children}{@render children()}{/if}
      </div>
      {#if sub}<div class="sub meta">{@render sub()}</div>{/if}
    </div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </div>
  {#if tabs}<div class="tabs-row">{@render tabs()}</div>{/if}
</header>

<style>
  .page-head {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    margin-bottom: var(--s5);
  }
  .page-head.has-tabs {
    margin-bottom: var(--s4);
  }
  .crumbs {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--s1);
    font-size: var(--t-sm);
    color: var(--fg-faint);
  }
  .crumbs a:hover {
    color: var(--accent-fg);
  }
  .sep {
    color: var(--line-strong);
  }
  .main-row {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--s4);
  }
  .left {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s1);
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
    min-height: var(--h-md);
  }
  h1 {
    overflow-wrap: anywhere;
  }
  .sub {
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
  .tabs-row {
    margin-top: var(--s3);
  }
  @media (max-width: 640px) {
    .main-row {
      flex-direction: column;
    }
    .actions {
      justify-content: flex-start;
    }
  }
</style>
