<script lang="ts">
  /**
   * 确认对话框。
   *
   * 用原生 `<dialog>`：模态语义、焦点收拢、Esc 关闭、`::backdrop` 都是浏览器
   * 给的，自己用 div 拼一遍只会拼出一个键盘用不了的版本。
   *
   * 和 `window.confirm` 的差别不只是好看：`confirm` 只能给一行纯文本，
   * 而**删任务这种事需要先把代价摆出来**——会连带删掉多少条执行记录、
   * 能不能撤销。那是结构化的信息，塞进一行字符串里没人会读。
   */
  let {
    open = $bindable(false),
    title,
    danger = false,
    confirmText = '确认',
    busy = false,
    onconfirm,
    children
  }: {
    open?: boolean;
    title: string;
    /** 不可逆的操作用红色确认键。 */
    danger?: boolean;
    confirmText?: string;
    /** 确认后的请求还在飞：按钮灰掉，免得点两次。 */
    busy?: boolean;
    onconfirm: () => void;
    children?: import('svelte').Snippet;
  } = $props();

  let el = $state<HTMLDialogElement | null>(null);

  // showModal() 才有焦点收拢和 ::backdrop，直接设 open 属性没有
  $effect(() => {
    if (!el) return;
    if (open && !el.open) el.showModal();
    if (!open && el.open) el.close();
  });

  function done(ok: boolean) {
    open = false;
    if (ok) onconfirm();
  }
</script>

<dialog bind:this={el} onclose={() => (open = false)} oncancel={() => (open = false)}>
  <h2>{title}</h2>
  <div class="body">{@render children?.()}</div>
  <footer>
    <button class="btn-ghost" onclick={() => done(false)} disabled={busy}>取消</button>
    <button class={danger ? 'btn-danger' : 'btn-primary'} onclick={() => done(true)} disabled={busy}>
      {confirmText}
    </button>
  </footer>
</dialog>

<style>
  dialog {
    width: min(28rem, calc(100vw - 2rem));
    padding: var(--s5);
    border: 1px solid var(--line-strong);
    border-radius: var(--r3);
    background: var(--surface-1);
    color: var(--fg);
    box-shadow: var(--shadow-pop);
  }
  dialog::backdrop {
    background: rgb(0 0 0 / 0.55);
    backdrop-filter: blur(2px);
  }
  /* 打开时轻轻推上来一下。没有过渡的话弹层是"闪"出来的，容易误点 */
  dialog[open] {
    animation: rise 0.14s ease-out;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
  h2 {
    margin: 0 0 var(--s3);
    font-size: 1.05rem;
  }
  .body {
    font-size: 0.86rem;
    line-height: 1.65;
    color: var(--fg-dim);
  }
  .body :global(b) {
    color: var(--fg);
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--s2);
    margin-top: var(--s5);
  }
</style>
