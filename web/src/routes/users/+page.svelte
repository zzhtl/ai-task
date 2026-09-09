<script lang="ts">
  // 用户与角色管理。整页都要 admin —— 后端也拦，这里只是别让人白点。
  import { api, ApiFailure } from '$api/client';
  import { session } from '$lib/auth/session.svelte';

  interface User {
    id: string;
    email: string;
    display_name: string;
    role: 'viewer' | 'operator' | 'admin';
    disabled: boolean;
    active_sessions: number;
    created_at: string;
  }

  const ROLES = ['viewer', 'operator', 'admin'] as const;
  /** 各档能做什么，写在界面上——不然"operator"是个没有含义的词。 */
  const WHAT_EACH_ROLE_CAN_DO: Record<(typeof ROLES)[number], string> = {
    viewer: '只能看',
    operator: '能建任务、触发执行、决策审批',
    admin: '再加上管主机、改策略、看审计、管人'
  };

  let users = $state<User[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  let email = $state('');
  let displayName = $state('');
  let password = $state('');
  let role = $state<(typeof ROLES)[number]>('operator');

  async function load() {
    try {
      users = (await api<{ items: User[] }>('/api/v1/users')).items;
      error = null;
    } catch (e) {
      error = describe(e);
    }
  }

  function describe(e: unknown): string {
    return e instanceof ApiFailure ? e.message : String(e);
  }

  async function act(run: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await run();
      await load();
    } catch (e) {
      error = describe(e);
    } finally {
      busy = false;
    }
  }

  const create = () =>
    act(async () => {
      await api('/api/v1/users', {
        method: 'POST',
        body: { email, display_name: displayName || email, password, role }
      });
      email = '';
      displayName = '';
      password = '';
    });

  const changeRole = (user: User, next: string) =>
    act(() => api(`/api/v1/users/${user.id}/role`, { method: 'PUT', body: { role: next } }));

  const toggleDisabled = (user: User) =>
    act(() =>
      api(`/api/v1/users/${user.id}/disabled`, {
        method: 'PUT',
        body: { disabled: !user.disabled }
      })
    );

  const revoke = (user: User) =>
    act(() => api(`/api/v1/users/${user.id}/revoke-sessions`, { method: 'POST' }));

  $effect(() => {
    void load();
  });
</script>

<header>
  <a href="/" class="muted">← 返回</a>
  <h1>用户</h1>
</header>

{#if !session.can('admin')}
  <p class="bad">需要管理员权限。</p>
{:else}
  {#if error}<p class="bad">{error}</p>{/if}

  <table>
    <thead>
      <tr>
        <th>邮箱</th><th>显示名</th><th>角色</th><th>会话</th><th>状态</th><th></th>
      </tr>
    </thead>
    <tbody>
      {#each users as user (user.id)}
        <tr class:disabled={user.disabled}>
          <td class="mono">{user.email}</td>
          <td>{user.display_name}</td>
          <td>
            <select
              value={user.role}
              disabled={busy}
              onchange={(e) => changeRole(user, e.currentTarget.value)}
            >
              {#each ROLES as r (r)}<option value={r}>{r}</option>{/each}
            </select>
          </td>
          <td>
            {user.active_sessions}
            {#if user.active_sessions > 0}
              <!-- 改口令不会让已发出的 token 失效，泄漏时要能主动踢下线 -->
              <button class="link" disabled={busy} onclick={() => revoke(user)}>踢下线</button>
            {/if}
          </td>
          <td>{user.disabled ? '已停用' : '正常'}</td>
          <td>
            <button disabled={busy} onclick={() => toggleDisabled(user)}>
              {user.disabled ? '恢复' : '停用'}
            </button>
          </td>
        </tr>
      {:else}
        <tr><td colspan="6" class="muted">还没有用户。</td></tr>
      {/each}
    </tbody>
  </table>

  <section class="new">
    <h2>加人</h2>
    <div class="row">
      <input bind:value={email} placeholder="邮箱" type="email" />
      <input bind:value={displayName} placeholder="显示名（可留空）" />
      <input bind:value={password} placeholder="口令（至少 12 个字符）" type="password" />
      <select bind:value={role}>
        {#each ROLES as r (r)}<option value={r}>{r}</option>{/each}
      </select>
      <button onclick={create} disabled={busy || !email || !password}>创建</button>
    </div>
    <p class="muted">{role}：{WHAT_EACH_ROLE_CAN_DO[role]}</p>
  </section>
{/if}

<style>
  h1 { margin: 0.5rem 0 1rem; font-size: 1.4rem; }
  h2 { font-size: 0.95rem; margin: 0 0 0.5rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.88rem; }
  th { text-align: left; color: var(--muted); font-weight: 500; padding: 0.3rem 0.6rem 0.3rem 0; }
  td { padding: 0.35rem 0.6rem 0.35rem 0; border-top: 1px solid var(--line); }
  tr.disabled { opacity: 0.5; }
  .new { margin-top: 1.5rem; }
  .row { display: flex; gap: 0.5rem; flex-wrap: wrap; }
  input, select, button {
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--line);
    border-radius: 0.4rem;
    background: var(--card);
    color: var(--fg);
    font: inherit;
  }
  input { flex: 1; min-width: 10rem; }
  button { cursor: pointer; }
  button:disabled { cursor: default; opacity: 0.5; }
  button.link { border: none; background: none; color: var(--muted); font-size: 0.8rem; padding: 0; }
  .muted { color: var(--muted); }
  .bad { color: var(--bad); }
  .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
</style>
