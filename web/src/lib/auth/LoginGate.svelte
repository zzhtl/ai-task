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
  <!-- 还没问过后端。这里不渲染任何东西：闪一下登录页再跳走比多等 100ms 更糟 -->
  <p class="muted">…</p>
{:else if session.identity === null && !session.authDisabled}
  <form onsubmit={submit}>
    <h1>ai-task</h1>
    <p class="sub">{firstRun ? '首次部署：创建管理员账号' : '登录'}</p>

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

    <button type="submit" disabled={busy}>{firstRun ? '创建并登录' : '登录'}</button>
    <button type="button" class="link" onclick={() => (firstRun = !firstRun)}>
      {firstRun ? '已有账号，去登录' : '还没有任何账号？创建管理员'}
    </button>
  </form>
{:else}
  {@render children?.()}
{/if}

<style>
  form {
    max-width: 22rem;
    margin: 4rem auto;
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  h1 { margin: 0; font-size: 1.4rem; }
  .sub { margin: 0 0 0.5rem; color: var(--muted); }
  input {
    padding: 0.5rem 0.7rem;
    border: 1px solid var(--line);
    border-radius: 0.4rem;
    background: var(--card);
    color: var(--fg);
    font: inherit;
  }
  button {
    padding: 0.5rem;
    border: 1px solid var(--line);
    border-radius: 0.4rem;
    background: var(--card);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  button[type='submit'] { border-color: var(--ok); color: var(--ok); }
  button.link { border: none; background: none; color: var(--muted); font-size: 0.85rem; }
  .bad { color: var(--bad); margin: 0; }
  .muted { color: var(--muted); text-align: center; margin: 4rem; }
</style>
