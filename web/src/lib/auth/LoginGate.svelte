<script lang="ts">
  /**
   * 未登录时挡在前面的登录页。
   *
   * 后端没开认证时（`AI_TASK_REQUIRE_AUTH=false`，回环上的默认）直接放行，
   * 不给单人自托管平添一道门。
   */
  import { ApiFailure } from '$api/client';
  import { session, refreshIdentity, login, bootstrap } from './session.svelte';

  let { children } = $props();

  let email = $state('');
  let password = $state('');
  let displayName = $state('');
  /** 首次部署：一个用户都没有，要先建管理员。 */
  let firstRun = $state(false);
  let error = $state<string | null>(null);
  let busy = $state(false);

  $effect(() => {
    void refreshIdentity();
  });

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    busy = true;
    error = null;
    try {
      if (firstRun) {
        await bootstrap(email, password, displayName || email);
      } else {
        await login(email, password);
      }
    } catch (e) {
      error = e instanceof ApiFailure ? e.message : String(e);
      // 409 = 已经有用户了，说明不是首次部署
      if (e instanceof ApiFailure && e.status === 409) firstRun = false;
    } finally {
      busy = false;
      password = '';
    }
  }
</script>

{#if session.identity === undefined}
  <!-- 还没问过后端。不渲染登录页：闪一下再跳走比多等 100ms 更糟 -->
  <div class="loading">…</div>
{:else if session.identity === null && !session.authDisabled}
  <div class="gate">
  <form onsubmit={submit}>
    <div class="brand"><span class="mark"></span><h1>ai-task</h1></div>
    <p class="sub">{firstRun ? '首次部署：创建管理员账号' : 'AI 执行控制平面'}</p>

    <input type="email" bind:value={email} placeholder="邮箱" required autocomplete="username" />
    {#if firstRun}
      <input bind:value={displayName} placeholder="显示名" />
    {/if}
    <input
      type="password"
      bind:value={password}
      placeholder={firstRun ? '口令（至少 12 个字符）' : '口令'}
      required
      autocomplete={firstRun ? 'new-password' : 'current-password'}
    />

    {#if error}<p class="bad">{error}</p>{/if}

    <button type="submit" class="btn-primary" disabled={busy}>
      {firstRun ? '创建并登录' : '登录'}
    </button>
    <button type="button" class="link" onclick={() => (firstRun = !firstRun)}>
      {firstRun ? '已有账号，去登录' : '还没有任何账号？创建管理员'}
    </button>
  </form>
  </div>
{:else}
  {@render children?.()}
{/if}

<style>
  .gate {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: var(--s4);
  }
  form {
    width: min(360px, 100%);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r4);
    padding: var(--s6);
  }
  .brand { display: flex; align-items: center; gap: var(--s2); }
  .mark {
    width: 10px; height: 10px; border-radius: 3px;
    background: var(--accent);
    box-shadow: 0 0 12px color-mix(in srgb, var(--accent) 60%, transparent);
  }
  h1 { font-size: 1.15rem; }
  .sub { margin: 0 0 var(--s2); color: var(--fg-dim); font-size: 0.85rem; }
  form button[type='submit'] { justify-content: center; }
  button.link {
    border: none; background: none; color: var(--fg-faint);
    font-size: 0.8rem; padding: 0; justify-content: center;
  }
  button.link:hover { background: none; color: var(--accent-fg); }
  .loading { min-height: 100vh; display: grid; place-items: center; color: var(--fg-faint); }
</style>
