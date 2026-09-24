<script lang="ts">
  /**
   * 未登录时挡在前面的登录页。
   *
   * 后端没开认证时（`AI_TASK_REQUIRE_AUTH=false`，回环上的默认）直接放行，
   * 不给单人自托管平添一道门。
   *
   * **会话中途过期时不换页。**以前是整页切回登录表单——那会卸载外壳和当前页面，
   * 编辑器里写了十分钟的提示词就没了。现在登录过之后再过期，页面原样留着，
   * 上面盖一层重新登录的弹层；登录回来接着干。
   */
  import { session, refreshIdentity } from './session.svelte';
  import LoginForm from './LoginForm.svelte';

  let { children } = $props();

  /** 这个页面上登录成功过。之后再出现"未登录"就是会话过期，不是首次打开。 */
  let hadSession = $state(false);
  /** 过期前是谁。重新登录时替他把邮箱填好。 */
  let lastEmail = $state('');

  $effect(() => {
    void refreshIdentity();
  });
  $effect(() => {
    if (session.identity) {
      hadSession = true;
      lastEmail = session.identity.email ?? '';
    }
  });

  const signedOut = $derived(session.identity === null && !session.authDisabled);

  let dialog = $state<HTMLDialogElement | null>(null);
  $effect(() => {
    if (!dialog) return;
    const want = signedOut && hadSession;
    if (want && !dialog.open) dialog.showModal();
    if (!want && dialog.open) dialog.close();
  });
</script>

{#if session.identity === undefined}
  <!-- 还没问过后端。不渲染登录页：闪一下再跳走比多等 100ms 更糟 -->
  <div class="loading"><span class="mark"></span></div>
{:else if signedOut && !hadSession}
  <div class="gate"><LoginForm /></div>
{:else}
  {@render children?.()}
  <!-- Esc 关不掉：不登录的话，下面的页面每个请求都是 401 -->
  <dialog bind:this={dialog} class="reauth" oncancel={(e) => e.preventDefault()}>
    {#if signedOut && hadSession}<LoginForm reauth email={lastEmail} />{/if}
  </dialog>
{/if}

<style>
  .gate {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: var(--s4);
    background:
      radial-gradient(60% 50% at 50% 0%, color-mix(in srgb, var(--accent) 14%, transparent), transparent 70%),
      var(--bg);
  }
  .reauth {
    padding: 0;
    border: none;
    background: transparent;
    overflow: visible;
  }
  .reauth::backdrop {
    background: rgb(0 0 0 / 0.55);
    backdrop-filter: blur(3px);
  }
  .loading {
    min-height: 100vh;
    display: grid;
    place-items: center;
  }
  .mark {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    background: var(--accent);
  }
</style>
