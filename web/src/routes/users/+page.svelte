<script lang="ts">
  // 用户与角色管理。整页都要 admin —— 后端也拦，这里只是别让人白点。
  import { api, describeError } from '$api/client';
  import { listUsers, type Role, type User } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, stamp } from '$lib/ui/format';

  const ROLES: Role[] = ['viewer', 'operator', 'admin'];
  /** 各档能做什么，写在界面上——不然"operator"是个没有含义的词。 */
  const WHAT_EACH_ROLE_CAN_DO: Record<Role, string> = {
    viewer: '只能看',
    operator: '能建任务、触发执行、决策审批',
    admin: '再加上管主机、改策略、看审计、管人'
  };

  let users = $state<User[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let busy = $state(false);
  let adding = $state(false);

  let email = $state('');
  let displayName = $state('');
  let password = $state('');
  let role = $state<Role>('operator');

  async function load() {
    try {
      users = await listUsers();
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  async function act(run: () => Promise<unknown>, done?: string) {
    busy = true;
    error = null;
    try {
      await run();
      await load();
      if (done) toast(done);
    } catch (e) {
      toastError(describeError(e));
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
      adding = false;
    }, '已创建用户');

  const changeRole = (user: User, next: string) =>
    act(
      () => api(`/api/v1/users/${user.id}/role`, { method: 'PUT', body: { role: next } }),
      `${user.display_name} 现在是 ${next}`
    );

  const toggleDisabled = (user: User) =>
    act(
      () =>
        api(`/api/v1/users/${user.id}/disabled`, {
          method: 'PUT',
          body: { disabled: !user.disabled }
        }),
      user.disabled ? '已恢复' : '已停用并吊销全部会话'
    );

  const revoke = (user: User) =>
    act(() => api(`/api/v1/users/${user.id}/revoke-sessions`, { method: 'POST' }), '已踢下线');

  $effect(() => {
    void load();
  });
</script>

<PageHeader title="用户">
  {#snippet sub()}
    <span>改角色立刻生效，不用重新登录；停用会连带吊销该用户所有会话。</span>
  {/snippet}
  {#snippet actions()}
    {#if session.can('admin') && !adding}
      <button class="btn-primary" onclick={() => (adding = true)}>添加用户</button>
    {/if}
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <div class="callout danger">需要管理员权限。</div>
{:else}
  {#if error}<div class="banner">{error}</div>{/if}

  {#if adding}
    <section class="card form">
      <header class="card-head">
        <h2>加人</h2>
        <span class="spacer"></span>
        <button class="btn-ghost btn-sm" onclick={() => (adding = false)}>收起</button>
      </header>
      <div class="form-grid">
        <label class="field">邮箱<input bind:value={email} type="email" placeholder="name@example.com" autocomplete="off" /></label>
        <label class="field">显示名<input bind:value={displayName} placeholder="可留空，默认用邮箱" autocomplete="off" /></label>
        <label class="field">
          初始口令
          <input bind:value={password} type="password" placeholder="至少 12 个字符" autocomplete="new-password" />
        </label>
        <label class="field">
          角色
          <select bind:value={role}>
            {#each ROLES as r (r)}<option value={r}>{r} — {WHAT_EACH_ROLE_CAN_DO[r]}</option>{/each}
          </select>
        </label>
      </div>
      <div class="form-actions">
        <button class="btn-primary" onclick={create} disabled={busy || !email || !password}>创建</button>
      </div>
    </section>
  {/if}

  {#if !loaded}
    <div class="card"><Loading rows={2} /></div>
  {:else}
    <div class="card flush">
      <table>
        <thead>
          <tr>
            <th>用户</th>
            <th>角色</th>
            <th>会话</th>
            <th>状态</th>
            <th>创建</th>
            <th class="act"></th>
          </tr>
        </thead>
        <tbody>
          {#each users as user (user.id)}
            <tr class:off={user.disabled}>
              <td>
                <div class="who">
                  <span class="avatar" class:admin={user.role === 'admin'}>
                    {user.display_name.trim().slice(0, 1).toUpperCase()}
                  </span>
                  <span class="col">
                    <span class="name">{user.display_name}</span>
                    <span class="mono faint small">{user.email}</span>
                  </span>
                </div>
              </td>
              <td>
                <select
                  value={user.role}
                  disabled={busy}
                  title={WHAT_EACH_ROLE_CAN_DO[user.role]}
                  onchange={(e) => changeRole(user, e.currentTarget.value)}
                >
                  {#each ROLES as r (r)}<option value={r}>{r}</option>{/each}
                </select>
              </td>
              <td>
                <span class="row">
                  <span>{user.active_sessions}</span>
                  {#if user.active_sessions > 0}
                    <!-- 改口令不会让已发出的 token 失效，泄漏时要能主动踢下线 -->
                    <button class="btn-ghost btn-sm" disabled={busy} onclick={() => revoke(user)}>踢下线</button>
                  {/if}
                </span>
              </td>
              <td>
                {#if user.disabled}<span class="tag danger">已停用</span>{:else}<span class="tag ok">正常</span>{/if}
              </td>
              <td class="faint" title={stamp(user.created_at)}>{ago(user.created_at)}</td>
              <td class="act">
                <button class="btn-sm" class:btn-danger={!user.disabled} disabled={busy} onclick={() => toggleDisabled(user)}>
                  {user.disabled ? '恢复' : '停用'}
                </button>
              </td>
            </tr>
          {:else}
            <tr><td colspan="6" class="muted">还没有用户。</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    margin-bottom: var(--s4);
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }
  .avatar {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    border: 1px solid var(--line-strong);
    font-size: 0.75rem;
    font-weight: 600;
    color: var(--fg-dim);
    flex: 0 0 auto;
  }
  .avatar.admin {
    border-color: var(--accent-dim);
    color: var(--accent-fg);
    background: color-mix(in srgb, var(--accent) 12%, var(--surface-3));
  }
  .col {
    display: flex;
    flex-direction: column;
    line-height: 1.3;
  }
  .name {
    font-weight: 500;
  }
  select {
    padding-top: 0.25rem;
    padding-bottom: 0.25rem;
  }
</style>
