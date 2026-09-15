<script lang="ts">
  // 用户与角色管理。整页都要 admin —— 后端也拦，这里只是别让人白点。
  import { api, describeError, fieldErrors } from '$api/client';
  import { listUsers, type Role, type User } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, stamp } from '$lib/ui/format';
  import Icon from '$lib/ui/Icon.svelte';

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
  /** 表单打开着：新建，或者在改某个人。 */
  let form = $state<'closed' | 'new' | User>('closed');
  /** 后端 422 里按字段拆出来的错误，显示在各自的框底下。 */
  let fieldErr = $state<Record<string, string>>({});
  const editing = $derived(typeof form === 'object' ? form : null);

  let email = $state('');
  let displayName = $state('');
  let password = $state('');
  let role = $state<Role>('operator');

  /** 是不是当前登录的这个人。没开认证时没有"我"。 */
  const isMe = (u: User) =>
    !!session.identity?.email && session.identity.email.toLowerCase() === u.email.toLowerCase();

  function openNew() {
    email = '';
    displayName = '';
    password = '';
    role = 'operator';
    fieldErr = {};
    form = 'new';
  }
  function openEdit(user: User) {
    email = user.email;
    displayName = user.display_name;
    password = '';
    fieldErr = {};
    form = user;
  }

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

  async function submit() {
    busy = true;
    error = null;
    fieldErr = {};
    try {
      if (editing) {
        await api(`/api/v1/users/${editing.id}`, {
          method: 'PUT',
          body: { email, display_name: displayName || email, password: password || null }
        });
        toast(password ? `已更新 ${displayName || email} 的资料和口令` : `已更新 ${displayName || email}`);
      } else {
        await api('/api/v1/users', {
          method: 'POST',
          body: { email, display_name: displayName || email, password, role }
        });
        toast('已创建用户');
      }
      form = 'closed';
      await load();
    } catch (e) {
      // 字段级的落到各自的框底下；其余（重名、最后一个管理员）留在页头横幅
      fieldErr = fieldErrors(e);
      error = Object.keys(fieldErr).length ? null : describeError(e);
    } finally {
      busy = false;
    }
  }

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

  let pendingDelete = $state<User | null>(null);
  const remove = (user: User) =>
    act(() => api(`/api/v1/users/${user.id}`, { method: 'DELETE' }), `已删除 ${user.display_name}`);

  $effect(() => {
    void load();
  });

  const canSubmit = $derived(!busy && !!email && (editing !== null || !!password));
</script>

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
  title="删除用户 {pendingDelete?.display_name ?? ''}？"
  danger
  confirmText="删除"
  {busy}
  onconfirm={() => {
    const user = pendingDelete;
    pendingDelete = null;
    if (user) void remove(user);
  }}
>
  <p><span class="mono">{pendingDelete?.email}</span> 会立刻登不进来，已有的会话全部失效。</p>
  <p>他建过的任务、做过的审批和审计记录保留，只是不再关联到这个人。</p>
</Confirm>

<PageHeader title="用户">
  {#snippet sub()}
    <span>改角色立刻生效，不用重新登录；停用会连带吊销该用户所有会话。</span>
  {/snippet}
  {#snippet actions()}
    {#if session.can('admin')}
      <button class="btn-primary" onclick={openNew}>添加用户</button>
    {/if}
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <div class="callout danger">需要管理员权限。</div>
{:else}
  {#if error}<div class="banner">{error}</div>{/if}

  <Modal
    open={form !== 'closed'}
    title={editing ? `编辑 ${editing.display_name}` : '添加用户'}
    onclose={() => (form = 'closed')}
  >
    <div class="form-grid">
      <Field label="邮箱" error={fieldErr.email}>
        {#snippet control(p)}
          <input {...p} bind:value={email} type="email" placeholder="name@example.com" autocomplete="off" />
        {/snippet}
      </Field>
      <Field label="显示名" error={fieldErr.display_name}>
        {#snippet control(p)}
          <input {...p} bind:value={displayName} placeholder="可留空，默认用邮箱" autocomplete="off" />
        {/snippet}
      </Field>
      <Field
        label={editing ? '新口令（留空不改）' : '初始口令'}
        hint={editing ? '改口令不会踢掉已登录的会话，需要的话在列表里点「踢下线」' : undefined}
        error={fieldErr.password}
        wide={!!editing}
      >
        {#snippet control(p)}
          <input {...p} bind:value={password} type="password" placeholder="至少 12 个字符" autocomplete="new-password" />
        {/snippet}
      </Field>
      {#if !editing}
        <Field label="角色" error={fieldErr.role}>
          {#snippet control(p)}
            <select {...p} bind:value={role}>
              {#each ROLES as r (r)}<option value={r}>{r} — {WHAT_EACH_ROLE_CAN_DO[r]}</option>{/each}
            </select>
          {/snippet}
        </Field>
      {/if}
    </div>
    {#snippet footer()}
      <button class="btn-ghost" onclick={() => (form = 'closed')} disabled={busy}>取消</button>
      <button class="btn-primary" onclick={submit} disabled={!canSubmit}>{editing ? '保存' : '创建'}</button>
    {/snippet}
  </Modal>

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
            {@const me = isMe(user)}
            <tr class:off={user.disabled} class:on={editing?.id === user.id}>
              <td>
                <div class="who">
                  <span class="avatar" class:admin={user.role === 'admin'}>
                    {user.display_name.trim().slice(0, 1).toUpperCase()}
                  </span>
                  <span class="col">
                    <span class="name">
                      {user.display_name}
                      {#if me}<span class="tag">我</span>{/if}
                      {#if user.system}
                        <span class="tag accent" title="首次部署创建的兜底账号：不能删、不能降级停用，资料只能由本人改">内置管理员</span>
                      {/if}
                    </span>
                    <span class="mono faint small">{user.email}</span>
                  </span>
                </div>
              </td>
              <td>
                <select
                  value={user.role}
                  disabled={busy || user.system}
                  title={user.system ? '内置管理员不能改角色' : WHAT_EACH_ROLE_CAN_DO[user.role]}
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
                <div class="row">
                  {#if !user.system || me || session.authDisabled}
                    <button class="btn-ghost btn-sm btn-icon" title="编辑资料" aria-label="编辑" disabled={busy} onclick={() => openEdit(user)}>
                      <Icon name="pencil" />
                    </button>
                  {/if}
                  {#if !user.system}
                    <button class="btn-sm" class:btn-danger={!user.disabled} disabled={busy} onclick={() => toggleDisabled(user)}>
                      {user.disabled ? '恢复' : '停用'}
                    </button>
                  {/if}
                  {#if !user.system && !me}
                    <button
                      class="btn-ghost btn-sm btn-icon danger"
                      title="删除用户"
                      aria-label="删除用户"
                      disabled={busy}
                      onclick={() => (pendingDelete = user)}
                    >
                      <Icon name="trash" />
                    </button>
                  {/if}
                </div>
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
    font-size: var(--t-xs);
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
    display: flex;
    align-items: center;
    gap: var(--s1);
    flex-wrap: wrap;
  }
  select {
    padding-top: 0.25rem;
    padding-bottom: 0.25rem;
  }
</style>
