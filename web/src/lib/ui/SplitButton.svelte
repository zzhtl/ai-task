<script lang="ts">
  /**
   * 主动作一次点完，旁边的小箭头里放次要动作。
   *
   * "运行"最常用的是立即执行：塞进下拉里就得点两次；影子执行也得有个入口，
   * 却又不值得单独占一个按钮。
   */
  import type { Snippet } from 'svelte';
  import Dropdown from './Dropdown.svelte';
  import Icon from './Icon.svelte';

  let {
    label,
    onclick,
    menuLabel,
    disabled = false,
    title,
    size = 'md',
    children
  }: {
    label: string;
    onclick: () => void;
    /** 箭头按钮的读屏名字，比如"更多运行方式"。 */
    menuLabel: string;
    disabled?: boolean;
    title?: string;
    size?: 'md' | 'sm';
    children: Snippet;
  } = $props();
</script>

<div class="split">
  <button class="btn-primary main" class:btn-sm={size === 'sm'} {disabled} {title} {onclick}>
    <Icon name="play" size={size === 'sm' ? 12 : 14} />
    {label}
  </button>
  <Dropdown label={menuLabel} primary {disabled} triggerClass="caret {size === 'sm' ? 'btn-sm' : ''}">
    {#snippet trigger()}<Icon name="chevron-down" size={12} />{/snippet}
    {@render children()}
  </Dropdown>
</div>

<style>
  .split {
    display: inline-flex;
    align-items: stretch;
  }
  .main {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }
  .split :global(.caret) {
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
    padding-inline: 0.45rem;
    /* 两半之间一道细缝：看得出是两个按钮，又还是一个整体 */
    border-left: 1px solid color-mix(in srgb, var(--accent-on) 30%, transparent);
  }
  .split :global(.caret svg) {
    margin: 0;
    opacity: 1;
  }
</style>
