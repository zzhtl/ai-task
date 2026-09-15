<script lang="ts">
  /**
   * 确认对话框。
   *
   * 用原生 `<dialog>`：模态语义、焦点收拢、Esc 关闭、`::backdrop` 都是浏览器
   * 给的，自己用 div 拼一遍只会拼出一个键盘用不了的版本。
   *
   * **受控组件**：`open` 是只读 prop，关闭一律走 `onclose`。
   * 曾经它是 `$bindable`，而九个调用点里有六个写成 `open={pendingDelete !== null}` 忘了 `bind:`——
   * 于是取消之后 `pendingDelete` 还留着，prop 表达式恒为 `true`、值不变化，
   * 再点另一行就再也弹不出来。把 `onclose` 设成必填之后，这个错误编不过去。
   *
   * 和 `window.confirm` 的差别不只是好看：`confirm` 只能给一行纯文本，
   * 而**删任务这种事需要先把代价摆出来**——会连带删掉多少条执行记录、
   * 能不能撤销。那是结构化的信息，塞进一行字符串里没人会读。
   */
  let {
    open,
    title,
    danger = false,
    confirmText = '确认',
    busy = false,
    onconfirm,
    onclose,
    children
  }: {
    open: boolean;
    title: string;
    /** 不可逆的操作用红色确认键。 */
    danger?: boolean;
    confirmText?: string;
    /** 确认后的请求还在飞：按钮灰掉，免得点两次。 */
    busy?: boolean;
    onconfirm: () => void;
    /** 取消 / Esc / 点背景 / 确认之后都会调。**必须**在这里清掉驱动 `open` 的那个状态。 */
    onclose: () => void;
    children?: import('svelte').Snippet;
  } = $props();

  let el = $state<HTMLDialogElement | null>(null);

  // showModal() 才有焦点收拢和 ::backdrop，直接设 open 属性没有
  $effect(() => {
    if (!el) return;
    if (open && !el.open) el.showModal();
    if (!open && el.open) el.close();
  });

  // 先 onconfirm 再 onclose：调用方的 onconfirm 普遍要读那个待确认的对象
  // （"删哪一台主机"），而 onclose 正是把它清掉的地方。反过来调，确认键就没东西可删了。
  function done(ok: boolean) {
    if (ok) onconfirm();
    onclose();
  }
</script>

<!-- 原生 close 事件覆盖 Esc 和点背景：cancel 之后浏览器照样会派发 close，一个就够 -->
<dialog bind:this={el} onclose={() => onclose()}>
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
    animation: rise var(--dur-2) var(--ease-out);
  }
  h2 {
    margin: 0 0 var(--s3);
    font-size: var(--t-lg);
  }
  .body {
    font-size: var(--t-base);
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
