<script lang="ts">
  /**
   * 下拉菜单。给"运行 ▾（立即 / 影子执行）"和"更多 ▾（停用 / 删除）"这类
   * 一个入口带几个动作的地方用。点外面或按 Esc 关掉。
   */
  import Icon from './Icon.svelte';

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
  let menu = $state<HTMLElement | null>(null);
  let trigger = $state<HTMLButtonElement | null>(null);

  function onDocClick(event: MouseEvent) {
    if (open && root && !root.contains(event.target as Node)) open = false;
  }

  /**
   * 菜单项由调用方以 snippet 传进来，所以角色和 tabindex 只能在打开时就地补。
   * 让调用方每处都记得写 `role="menuitem"` 是行不通的——九个调用点里之前只有两个写了。
   */
  function items(): HTMLElement[] {
    if (!menu) return [];
    return [...menu.querySelectorAll<HTMLElement>('button, a[href]')];
  }

  $effect(() => {
    if (!open || !menu) return;
    const list = items();
    for (const item of list) {
      item.setAttribute('role', 'menuitem');
      // 菜单内部走方向键，不走 Tab：Tab 应该直接离开整个菜单
      item.tabIndex = -1;
    }
    list[0]?.focus();
  });

  function close(returnFocus: boolean) {
    open = false;
    if (returnFocus) trigger?.focus();
  }

  /** 菜单内的方向键漫游。 */
  function onMenuKey(event: KeyboardEvent) {
    const list = items();
    if (list.length === 0) return;
    const at = list.indexOf(document.activeElement as HTMLElement);
    const go = (i: number) => {
      event.preventDefault();
      list[(i + list.length) % list.length]?.focus();
    };
    if (event.key === 'ArrowDown') go(at + 1);
    else if (event.key === 'ArrowUp') go(at - 1);
    else if (event.key === 'Home') go(0);
    else if (event.key === 'End') go(list.length - 1);
    else if (event.key === 'Tab') close(false);
  }

  function onTriggerKey(event: KeyboardEvent) {
    if (event.key === 'ArrowDown' && !open) {
      event.preventDefault();
      open = true;
    }
  }

  function onKey(event: KeyboardEvent) {
    // Esc 关掉并把焦点还给触发按钮，否则焦点会掉到 body 上，键盘用户就丢了位置
    if (event.key === 'Escape' && open) close(true);
  }
</script>

<svelte:document onclick={onDocClick} onkeydown={onKey} />

<div class="dd" bind:this={root}>
  <button
    bind:this={trigger}
    class:btn-primary={primary}
    {disabled}
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={() => (open = !open)}
    onkeydown={onTriggerKey}
  >
    {label}
    <Icon name="chevron-down" size={12} />
  </button>
  {#if open}
    <!-- 菜单项自己是 button / a；点了任何一项都关菜单，键盘走 onMenuKey 的方向键漫游 -->
    <div
      bind:this={menu}
      class="menu {align}"
      role="menu"
      tabindex="-1"
      onclick={() => close(true)}
      onkeydown={onMenuKey}
    >
      {@render children()}
    </div>
  {/if}
</div>

<style>
  .dd {
    position: relative;
    display: inline-flex;
  }
  /* 图标搬进子组件后 scoped 选择器就够不到了；尺寸走 Icon 的 size 属性 */
  .dd > button :global(svg) {
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
    animation: drop var(--dur-2) var(--ease-out);
  }
  .menu.right {
    right: 0;
  }
  .menu.left {
    left: 0;
  }
  @keyframes drop {
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
    font-size: var(--t-base);
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
    font-size: var(--t-xs);
    color: var(--fg-faint);
    font-weight: 400;
  }
  .menu :global(hr) {
    border: none;
    border-top: 1px solid var(--line);
    margin: 3px 0;
  }
</style>
