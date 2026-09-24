<script lang="ts">
  /**
   * 复制按钮。给了 `display` 就是一段可点的等宽文字（短 id），没给就是个图标。
   * 点完原地变成对勾——复制成功与否不该靠弹一个 toast 来说。
   */
  import Icon from './Icon.svelte';
  import { toastError } from './toast.svelte';

  let {
    text,
    display,
    label = '复制'
  }: {
    /** 真正写进剪贴板的内容。 */
    text: string;
    /** 显示出来的文字，比如 id 的前 8 位。 */
    display?: string;
    label?: string;
  } = $props();

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function copy(event: MouseEvent) {
    // 常放在可点击的表格行里：复制不该顺带打开那一行
    event.stopPropagation();
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => (copied = false), 1200);
    } catch {
      toastError('复制失败：浏览器没给剪贴板权限');
    }
  }
</script>

{#if display}
  <button class="chip mono" type="button" title="{label}：{text}" aria-label="{label} {text}" onclick={copy}>
    {display}
    <Icon name={copied ? 'check' : 'copy'} />
  </button>
{:else}
  <button
    class="btn-ghost btn-sm btn-icon"
    type="button"
    title={copied ? '已复制' : label}
    aria-label={label}
    onclick={copy}
  >
    <Icon name={copied ? 'check' : 'copy'} />
  </button>
{/if}

<style>
  .chip,
  :global(:root[data-theme='light']) .chip {
    height: auto;
    gap: 0.3rem;
    padding: 0.1rem 0.35rem;
    border: none;
    border-radius: var(--r1);
    background: transparent;
    box-shadow: none;
    color: var(--fg-faint);
    font-size: var(--t-xs);
    font-weight: 400;
  }
  .chip:hover:not(:disabled) {
    background: var(--surface-3);
    color: var(--fg);
  }
  .chip :global(svg) {
    width: 12px;
    height: 12px;
    opacity: 0.7;
  }
</style>
