<script lang="ts">
  /**
   * 一个"?"，悬停或聚焦时弹出一段说明。
   *
   * 页面上原来到处是大段的解释性文字——每次打开都得从它上面读过去，
   * 真正要看的数据反而被挤到了下面。说明收到这里：要看的人一悬停就有。
   *
   * 气泡放进 Popover API 的顶层（top layer）：表格和卡片的 `overflow: hidden`
   * 裁不到它，也不用操心 z-index。
   */
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    text,
    label = '说明',
    children
  }: {
    /** 纯文字说明。要带格式（加粗、代码）就用 children。 */
    text?: string;
    /** 读屏念的名字。 */
    label?: string;
    children?: Snippet;
  } = $props();

  const id = $props.id();
  let anchor = $state<HTMLButtonElement | null>(null);
  let bubble = $state<HTMLDivElement | null>(null);
  /** 点过就钉住，直到再点一次或按 Esc：触屏上没有悬停，只能靠点。 */
  let pinned = $state(false);

  function show() {
    if (!anchor || !bubble || bubble.matches(':popover-open')) return;
    bubble.showPopover();
    place();
  }
  function hide() {
    if (pinned) return;
    bubble?.hidePopover();
  }
  function toggle() {
    pinned = !pinned;
    if (pinned) show();
    else bubble?.hidePopover();
  }

  /** 放在图标下方；贴边时往回收，别跑出视口。 */
  function place() {
    if (!anchor || !bubble) return;
    const a = anchor.getBoundingClientRect();
    const b = bubble.getBoundingClientRect();
    const margin = 8;
    let left = a.left + a.width / 2 - b.width / 2;
    left = Math.max(margin, Math.min(left, window.innerWidth - b.width - margin));
    let top = a.bottom + 6;
    // 下面放不下就放到上面
    if (top + b.height > window.innerHeight - margin) top = a.top - b.height - 6;
    bubble.style.left = `${left}px`;
    bubble.style.top = `${Math.max(margin, top)}px`;
  }

  function onKey(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      pinned = false;
      bubble?.hidePopover();
    }
  }
</script>

<button
  bind:this={anchor}
  class="help"
  type="button"
  aria-label={label}
  aria-describedby={id}
  aria-expanded={pinned}
  onmouseenter={show}
  onmouseleave={hide}
  onfocus={show}
  onblur={() => {
    pinned = false;
    bubble?.hidePopover();
  }}
  onclick={toggle}
  onkeydown={onKey}
>
  <Icon name="help" />
</button>
<div bind:this={bubble} {id} popover="manual" role="tooltip" class="bubble">
  {#if children}{@render children()}{:else}{text}{/if}
</div>

<style>
  .help,
  :global(:root[data-theme='light']) .help {
    display: inline-grid;
    place-items: center;
    width: 1.125rem;
    height: 1.125rem;
    padding: 0;
    border: none;
    border-radius: 50%;
    background: transparent;
    box-shadow: none;
    color: var(--fg-faint);
    vertical-align: middle;
    cursor: help;
  }
  .help:hover:not(:disabled),
  .help[aria-expanded='true'] {
    background: var(--surface-3);
    color: var(--fg);
  }
  .help :global(svg) {
    width: 14px;
    height: 14px;
  }
  .bubble {
    position: fixed;
    inset: auto;
    margin: 0;
    max-width: min(22rem, calc(100vw - 16px));
    padding: var(--s2) var(--s3);
    border: 1px solid var(--line-strong);
    border-radius: var(--r2);
    background: var(--surface-1);
    color: var(--fg-dim);
    box-shadow: var(--shadow-pop);
    font-size: var(--t-sm);
    font-weight: 400;
    line-height: 1.6;
    text-align: left;
    white-space: normal;
  }
  .bubble :global(strong),
  .bubble :global(b) {
    color: var(--fg);
  }
</style>
