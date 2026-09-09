<script lang="ts">
  /**
   * 下拉菜单。给"运行 ▾（立即 / 影子执行）"和"更多 ▾（停用 / 删除）"这类
   * 一个入口带几个动作的地方用。点外面或按 Esc 关掉。
   */
  let {
    label,
    primary = false,
    disabled = false,
    align = 'right',
    children
  }: {
    label: string;
    primary?: boolean;
    disabled?: boolean;
    align?: 'left' | 'right';
    children: import('svelte').Snippet;
  } = $props();

  let open = $state(false);
  let root = $state<HTMLElement | null>(null);

  function onDocClick(event: MouseEvent) {
    if (open && root && !root.contains(event.target as Node)) open = false;
  }
  function onKey(event: KeyboardEvent) {
    if (event.key === 'Escape') open = false;
  }
</script>

<svelte:document onclick={onDocClick} onkeydown={onKey} />

<div class="dd" bind:this={root}>
  <button
    class:btn-primary={primary}
    {disabled}
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={() => (open = !open)}
  >
    {label}
    <svg viewBox="0 0 24 24"><path d="M6 9l6 6 6-6" /></svg>
  </button>
  {#if open}
    <!-- 菜单项自己是 button；点了任何一项都关菜单 -->
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
    <div class="menu {align}" role="menu" tabindex="-1" onclick={() => (open = false)}>
      {@render children()}
    </div>
  {/if}
</div>

<style>
  .dd {
    position: relative;
    display: inline-flex;
  }
  .dd > button svg {
    width: 12px;
    height: 12px;
    margin-left: -2px;
    opacity: 0.7;
  }
  .menu {
    position: absolute;
    top: calc(100% + 4px);
    min-width: 12rem;
    padding: 4px;
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--r2);
    box-shadow: var(--shadow-pop);
    z-index: var(--z-pop);
    display: flex;
    flex-direction: column;
    gap: 1px;
    animation: rise 0.12s ease-out;
  }
  .menu.right {
    right: 0;
  }
  .menu.left {
    left: 0;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
  }
  /* 菜单项可以是 button 也可以是 a（跳转类动作）。两者长得必须一样 */
  .menu :global(:is(button, a)) {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    width: 100%;
    border: none;
    background: transparent;
    box-shadow: none;
    color: var(--fg);
    font-size: 0.85rem;
    line-height: 1.3;
    text-align: left;
    padding: 0.45rem 0.6rem;
    border-radius: 6px;
    white-space: normal;
    cursor: pointer;
  }
  .menu :global(:is(button, a):hover:not(:disabled)) {
    background: var(--surface-3);
    color: var(--fg);
  }
  .menu :global(:is(button, a).danger) {
    color: var(--bad);
  }
  .menu :global(:is(button, a) .hint) {
    font-size: 0.72rem;
    color: var(--fg-faint);
    font-weight: 400;
  }
  .menu :global(hr) {
    border: none;
    border-top: 1px solid var(--line);
    margin: 3px 0;
  }
</style>
