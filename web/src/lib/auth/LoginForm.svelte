<script lang="ts">
  /**
   * 登录表单。首次打开时整页用它；会话中途过期时放在弹层里用它。
   *
   * 首次部署（一个用户都没有）时可以切到"创建管理员"；重新登录的弹层里不给这个入口。
   */
  import { ApiFailure, describeError } from '$api/client';
  import { login, bootstrap } from './session.svelte';

  let {
    reauth = false,
    email: initialEmail = ''
  }: {
    /** 会话过期后的重新登录：换标题，不给"创建管理员"。 */
    reauth?: boolean;
    email?: string;
  } = $props();

  // 只取初始值：之后是用户自己在改
  // svelte-ignore state_referenced_locally
  let email = $state(initialEmail);
  let password = $state('');
  let displayName = $state('');
  /** 首次部署：一个用户都没有，要先建管理员。 */
  let firstRun = $state(false);
  let error = $state<string | null>(null);
  let busy = $state(false);

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

<form onsubmit={submit}>
  <div class="brand"><span class="mark"></span><h1>{reauth ? '会话已过期' : 'ai-task'}</h1></div>
  <p class="sub">
    {#if reauth}
      重新登录后继续。这一页上没保存的内容还在。
    {:else}
      {firstRun ? '首次部署：创建管理员账号' : 'AI 执行控制平面'}
    {/if}
  </p>

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
  {#if !reauth}
    <button type="button" class="link" onclick={() => (firstRun = !firstRun)}>
      {firstRun ? '已有账号，去登录' : '还没有任何账号？创建管理员'}
    </button>
  {/if}
</form>

<style>
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
    font-size: var(--t-xl);
  }
  .sub {
    margin: 0 0 var(--s2);
    color: var(--fg-dim);
    font-size: var(--t-base);
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
    font-size: var(--t-sm);
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
</style>
