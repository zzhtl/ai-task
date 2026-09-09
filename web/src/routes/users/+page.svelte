<script lang="ts">
  // 用户与角色管理。整页都要 admin —— 后端也拦，这里只是别让人白点。
  import { api, ApiFailure } from '$api/client';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';

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

<PageHeader title="用户">
  {#snippet sub()}
    <span>改角色立刻生效，不用重新登录；停用会连带吊销该用户所有会话。</span>
  {/snippet}
</PageHeader>

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
              <button class="btn-ghost btn-sm" disabled={busy} onclick={() => revoke(user)}>踢下线</button>
            {/if}
          </td>
          <td>{#if user.disabled}<span class="tag danger">已停用</span>{:else}<span class="tag ok">正常</span>{/if}</td>
          <td>
            <button class="btn-sm" disabled={busy} onclick={() => toggleDisabled(user)}>
              {user.disabled ? '恢复' : '停用'}
            </button>
          </td>
        </tr>
      {:else}
        <tr><td colspan="6" class="muted">还没有用户。</td></tr>
      {/each}
    </tbody>
  </table>

  <section class="new card">
    <h2>加人</h2>
    <div class="row">
      <input bind:value={email} placeholder="邮箱" type="email" />
      <input bind:value={displayName} placeholder="显示名（可留空）" />
      <input bind:value={password} placeholder="口令（至少 12 个字符）" type="password" />
      <select bind:value={role}>
        {#each ROLES as r (r)}<option value={r}>{r}</option>{/each}
      </select>
      <button class="btn-primary" onclick={create} disabled={busy || !email || !password}>创建</button>
    </div>
    <p class="muted">{role}：{WHAT_EACH_ROLE_CAN_DO[role]}</p>
  </section>
{/if}

<style>
  .new { margin-top: var(--s5); }
  .row { display: flex; gap: var(--s2); flex-wrap: wrap; align-items: center; }
  .row input { flex: 1; min-width: 10rem; }
  tr.disabled { opacity: 0.5; }
</style>
