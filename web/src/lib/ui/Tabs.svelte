<script lang="ts" generics="T extends string">
  /**
   * 页签。当前页签写进 URL 的 `?tab=`：链接能直接贴给同事，刷新也不丢位置。
   *
   * **状态放在组件里，只在初始化时读一次 URL。**写回用 kit 的浅路由 `replaceState`：
   * 它不会触发导航，也就不会撞上编辑页的"未保存离开"拦截；代价是它不更新
   * `page.url`，所以之后不能再从 `page.url` 读。
   */
  import { page } from '$app/state';
  import { replaceState } from '$app/navigation';
  import { untrack } from 'svelte';

  let {
    tabs,
    value = $bindable(),
    param = 'tab'
  }: {
    tabs: Array<{ id: T; label: string; count?: number | null }>;
    value: T;
    /** 写进 URL 的参数名。同一页有两组页签时各用一个。 */
    param?: string;
  } = $props();

  // 只取初始值：之后的切换由用户点击驱动
  untrack(() => {
    const fromUrl = page.url.searchParams.get(param);
    const hit = tabs.find((t) => t.id === fromUrl);
    if (hit) value = hit.id;
  });

  const id = $props.id();
  let list = $state<HTMLElement | null>(null);

  function select(next: T) {
    if (next === value) return;
    value = next;
    const url = new URL(location.href);
    // 默认页签不写进 URL，免得每个链接都拖着一个 ?tab=
    if (next === tabs[0]?.id) url.searchParams.delete(param);
    else url.searchParams.set(param, next);
    try {
      replaceState(url, page.state);
    } catch {
      // 路由还没起来（极早期的点击）：URL 不同步不影响切换本身
    }
  }

  /** 方向键在页签之间移动（WAI-ARIA 的 tabs 模式），Tab 键直接离开整组。 */
  function onKey(event: KeyboardEvent) {
    const at = tabs.findIndex((t) => t.id === value);
    let to = -1;
    if (event.key === 'ArrowRight') to = (at + 1) % tabs.length;
    else if (event.key === 'ArrowLeft') to = (at - 1 + tabs.length) % tabs.length;
    else if (event.key === 'Home') to = 0;
    else if (event.key === 'End') to = tabs.length - 1;
    if (to < 0) return;
    event.preventDefault();
    select(tabs[to].id);
    list?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[to]?.focus();
  }
</script>

<div class="tabs" role="tablist" bind:this={list} tabindex="-1" onkeydown={onKey}>
  {#each tabs as tab (tab.id)}
    <button
      type="button"
      role="tab"
      id="{id}-{tab.id}"
      aria-selected={tab.id === value}
      tabindex={tab.id === value ? 0 : -1}
      class:on={tab.id === value}
      onclick={() => select(tab.id)}
    >
      {tab.label}
      {#if tab.count !== undefined && tab.count !== null}<span class="count">{tab.count}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: var(--s4);
    border-bottom: 1px solid var(--line);
    overflow-x: auto;
    scrollbar-width: none;
  }
  .tabs:focus-visible {
    outline: none;
  }
  button,
  :global(:root[data-theme='light']) .tabs button {
    position: relative;
    height: 2.5rem;
    padding: 0 0.1rem;
    border: none;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
    color: var(--fg-dim);
    font-size: var(--t-base);
    font-weight: 500;
  }
  button:hover:not(:disabled) {
    background: transparent;
    color: var(--fg);
  }
  button.on {
    color: var(--fg);
  }
  /* 当前页签下面一条强调色的线，压在整组的底线上 */
  button.on::after {
    content: '';
    position: absolute;
    left: 0;
    right: 0;
    bottom: -1px;
    height: 2px;
    border-radius: 2px 2px 0 0;
    background: var(--accent);
  }
  button:focus-visible {
    outline-offset: -2px;
  }
  .count {
    margin-left: 0.35rem;
    padding: 0 0.4rem;
    border-radius: 999px;
    background: var(--surface-3);
    color: var(--fg-dim);
    font-size: var(--t-xs);
    line-height: 1.2rem;
  }
</style>
