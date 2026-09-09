<script lang="ts">
  /**
   * 未登录时挡在前面的登录页。
   *
   * 后端没开认证时（`AI_TASK_REQUIRE_AUTH=false`，回环上的默认）直接放行，
   * 不给单人自托管平添一道门。
   */
  import { ApiFailure, describeError } from '$api/client';
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
      error = describeError(e);
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
  <div class="loading"><span class="mark"></span></div>
{:else if session.identity === null && !session.authDisabled}
  <div class="gate">
    <form onsubmit={submit}>
      <div class="brand"><span class="mark"></span><h1>ai-task</h1></div>
      <p class="sub">{firstRun ? '首次部署：创建管理员账号' : 'AI 执行控制平面'}</p>

      <label class="field">
        邮箱
        <input type="email" bind:value={email} required autocomplete="username" />
      </label>
      {#if firstRun}
        <label class="field">
          显示名
          <input bind:value={displayName} placeholder="可留空，默认用邮箱" />
        </label>
      {/if}
      <label class="field">
        口令
        <input
          type="password"
          bind:value={password}
          placeholder={firstRun ? '至少 12 个字符' : ''}
          required
          autocomplete={firstRun ? 'new-password' : 'current-password'}
        />
      </label>

      {#if error}<div class="banner">{error}</div>{/if}

      <button type="submit" class="btn-primary" disabled={busy}>
        {busy ? '请稍候…' : firstRun ? '创建并登录' : '登录'}
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
    background:
      radial-gradient(60% 50% at 50% 0%, color-mix(in srgb, var(--accent) 14%, transparent), transparent 70%),
      var(--bg);
  }
  form {
    width: min(380px, 100%);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r4);
    padding: var(--s6);
    box-shadow: var(--shadow-pop);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }
  .mark {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    background: var(--accent);
    box-shadow: 0 0 14px color-mix(in srgb, var(--accent) 60%, transparent);
    animation: breathe 3.5s ease-in-out infinite;
  }
  @keyframes breathe {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.55;
      transform: scale(0.86);
    }
  }
  h1 {
    font-size: 1.2rem;
  }
  .sub {
    margin: 0 0 var(--s2);
    color: var(--fg-dim);
    font-size: 0.85rem;
  }
  form button[type='submit'] {
    justify-content: center;
    padding: 0.55rem;
    margin-top: var(--s1);
  }
  button.link {
    border: none;
    background: none;
    color: var(--fg-faint);
    font-size: 0.8rem;
    padding: 0;
    justify-content: center;
  }
  button.link:hover {
    background: none;
    color: var(--accent-fg);
  }
  .banner {
    margin: 0;
  }
  .loading {
    min-height: 100vh;
    display: grid;
    place-items: center;
  }
</style>
