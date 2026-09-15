<script lang="ts">
  /**
   * 弹层。原生 `<dialog>` + `showModal()`：模态语义、焦点收拢、Esc、`::backdrop`
   * 都是浏览器给的，自己用 div 拼一遍只会拼出一个键盘用不了的版本。
   *
   * **受控**：`open` 只读，关闭一律走 `onclose`——忘了写编译就报错。
   * 这条规矩是有来历的：`Confirm` 曾经是 `$bindable`，九个调用点里六个忘了 `bind:`，
   * 于是取消之后再点另一行就永远弹不出来。
   *
   * 和 `Confirm` 的分工：`Confirm` 是"问一句要不要"，这个能装下整个表单。
   * 之前没有它，所以主机 / 规则 / 用户 / 定时的新增表单只能内联展开在列表上方。
   */
  import Icon from './Icon.svelte';

  let {
    open,
    title,
    /** 宽度档位。表单用 wide，一句话的确认用 narrow。 */
    size = 'md',
    onclose,
    children,
    footer
  }: {
    open: boolean;
    title: string;
    size?: 'sm' | 'md' | 'lg';
    onclose: () => void;
    children: import('svelte').Snippet;
    footer?: import('svelte').Snippet;
  } = $props();

  let el = $state<HTMLDialogElement | null>(null);

  $effect(() => {
    if (!el) return;
    if (open && !el.open) el.showModal();
    if (!open && el.open) el.close();
  });
</script>

<!-- 点 ::backdrop 时 e.target 就是 dialog 本身；原生 dialog 认背景点击就靠这个 -->
<dialog
  bind:this={el}
  class={size}
  onclose={() => onclose()}
  onclick={(event) => {
    if (event.target === el) onclose();
  }}
>
  <header>
    <h2>{title}</h2>
    <button class="btn-ghost btn-icon btn-sm" aria-label="关闭" onclick={() => onclose()}>
      <Icon name="close" />
    </button>
  </header>
  <div class="body">{@render children()}</div>
  {#if footer}
    <footer>{@render footer()}</footer>
  {/if}
</dialog>

<style>
  dialog {
    padding: 0;
    margin: auto;
    max-height: min(85vh, 48rem);
    border: 1px solid var(--line-strong);
    border-radius: var(--r3);
    background: var(--surface-1);
    color: var(--fg);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
  }
  /* **display 只能写在 [open] 上。** 写在裸 dialog 上会盖掉浏览器默认的
     `display: none`，于是关着的弹层照样铺在页面上——而 `el.open` 仍然是 false，
     看起来像"弹层自己弹出来了"，实际是它从来没被藏起来过。 */
  dialog[open] {
    display: flex;
    flex-direction: column;
  }
  dialog.sm {
    width: min(28rem, calc(100vw - 2rem));
  }
  dialog.md {
    width: min(38rem, calc(100vw - 2rem));
  }
  dialog.lg {
    width: min(52rem, calc(100vw - 2rem));
  }
  dialog::backdrop {
    background: rgb(0 0 0 / 0.55);
    backdrop-filter: blur(2px);
  }
  /* 没有过渡的话弹层是"闪"出来的，容易误点 */
  dialog[open] {
    animation: rise var(--dur-2) var(--ease-out);
  }

  header {
    display: flex;
    align-items: center;
    gap: var(--s2);
    padding: var(--s4) var(--s5);
    border-bottom: 1px solid var(--line);
  }
  header h2 {
    margin: 0;
    font-size: var(--t-lg);
    flex: 1;
    min-width: 0;
  }
  .body {
    padding: var(--s5);
    overflow-y: auto;
    /* 表单比屏幕长时滚动的是内容，不是整个弹层 */
    flex: 1;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--s2);
    padding: var(--s3) var(--s5) var(--s4);
    border-top: 1px solid var(--line);
    background: var(--surface-2);
  }
</style>
