<script lang="ts">
  /**
   * SSH 主机。任务要下发到别的机器，先在这里加一台。
   *
   * 私钥只进不出：加密后落库，任何读接口都不返回它。这个页面也永远显示不出来。
   */
  import { api, describeError } from '$api/client';
  import { listHosts, type Host } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast } from '$lib/ui/toast.svelte';
  import { ago, stamp } from '$lib/ui/format';

  let hosts = $state<Host[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let busy = $state(false);
  let adding = $state(false);

  let name = $state('');
  let address = $state('');
  let port = $state(22);
  let username = $state('');
  let tags = $state('');
  let privateKey = $state('');

  async function load() {
    try {
      hosts = await listHosts();
      error = null;
      if (hosts.length === 0) adding = true;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  async function create() {
    busy = true;
    error = null;
    try {
      await api('/api/v1/hosts', {
        method: 'POST',
        body: {
          name,
          address,
          port,
          username,
          tags: tags
            .split(',')
            .map((t) => t.trim())
            .filter(Boolean),
          private_key: privateKey
        }
      });
      toast(`已添加主机「${name}」`);
      name = '';
      address = '';
      username = '';
      tags = '';
      privateKey = '';
      adding = false;
      await load();
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    void load();
  });

  const CGROUP: Record<string, string> = {
    systemd: 'cgroup 上限已强制',
    proc: '只能按 /proc 采样，上限未强制',
    none: '采不到资源，上限未强制'
  };
</script>

<PageHeader title="主机">
  {#snippet sub()}
    <span>任务要下发到别的机器，先在这里加一台。私钥加密落库，任何读接口都不返回。</span>
  {/snippet}
  {#snippet actions()}
    {#if session.can('admin') && !adding}
      <button class="btn-primary" onclick={() => (adding = true)}>添加主机</button>
    {/if}
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <div class="callout danger">需要管理员权限：这里存的是 SSH 私钥。</div>
{:else}
  {#if error}<div class="banner">{error}</div>{/if}

  {#if adding}
    <section class="card form">
      <header class="card-head">
        <h2>加一台</h2>
        <span class="spacer"></span>
        {#if hosts.length}
          <button class="btn-ghost btn-sm" onclick={() => (adding = false)}>收起</button>
        {/if}
      </header>
      <div class="form-grid">
        <label class="field">名称<input bind:value={name} placeholder="prod-web-1" /></label>
        <label class="field">地址或 IP<input bind:value={address} placeholder="10.0.0.12" spellcheck="false" /></label>
        <label class="field narrow">端口<input bind:value={port} type="number" min="1" max="65535" /></label>
        <label class="field">登录用户名<input bind:value={username} placeholder="deploy" spellcheck="false" /></label>
        <label class="field">
          tag
          <input bind:value={tags} placeholder="prod, web（逗号分隔）" />
          <span class="hint">策略可以按 tag 生效</span>
        </label>
      </div>
      <label class="field">
        OpenSSH 私钥
        <textarea
          bind:value={privateKey}
          spellcheck="false"
          rows="6"
          placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
        ></textarea>
      </label>
      <p class="faint small">
        首次连接会记下对方的主机密钥（TOFU）；<strong>密钥变了永远是拒绝</strong>——那是中间人攻击的信号。
        连上之后会探测目标机上有没有 systemd cgroup、装了哪些 AI CLI。
      </p>
      <div class="form-actions">
        <button
          class="btn-primary"
          onclick={create}
          disabled={busy || !name || !address || !username || !privateKey}
        >
          添加主机
        </button>
      </div>
    </section>
  {/if}

  {#if !loaded}
    <div class="card"><Loading rows={2} /></div>
  {:else if hosts.length}
    <div class="card flush">
      <table>
        <thead>
          <tr>
            <th>名称</th>
            <th>地址</th>
            <th>tag</th>
            <th>可直接跑的 AI</th>
            <th>资源归因</th>
            <th>agent</th>
            <th>最近连接</th>
          </tr>
        </thead>
        <tbody>
          {#each hosts as host (host.id)}
            <tr>
              <td class="name">{host.name}</td>
              <td class="mono">{host.username}@{host.address}:{host.port}</td>
              <td>
                {#if host.tags.length}
                  {#each host.tags as t (t)}<span class="tag">{t}</span>{/each}
                {:else}
                  <span class="faint">—</span>
                {/if}
              </td>
              <td>
                <!-- 决定这台机器能不能选"目标机执行"。没探测过和探测到没有，
                     是两回事——前者不能显示成"没有" -->
                {#if host.ai_clis?.length}
                  {#each host.ai_clis as cli (cli.name)}
                    <span class="tag accent" title={cli.path}>{cli.name}</span>
                  {/each}
                {:else if host.last_seen_at}
                  <span class="faint">没有</span>
                {:else}
                  <span class="faint">未探测</span>
                {/if}
              </td>
              <td>
                {#if host.cgroup_mode === null}
                  <span class="faint">未探测</span>
                {:else if host.degraded}
                  <!-- 这一档下 limits 根本没被强制，不标出来用户会以为限额生效了 -->
                  <span class="tag warn" title={CGROUP[host.cgroup_mode] ?? ''}>{host.cgroup_mode} · 上限未强制</span>
                {:else}
                  <span class="tag ok" title={CGROUP[host.cgroup_mode] ?? ''}>{host.cgroup_mode}</span>
                {/if}
              </td>
              <td class="mono faint">{host.agent_version?.slice(0, 8) ?? '—'}</td>
              <td class="faint" title={stamp(host.last_seen_at)}>{ago(host.last_seen_at)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else if !adding}
    <Empty title="还没有主机" hint="加一台之后，任务步骤里就能选择在它上面执行。" />
  {/if}
{/if}

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    margin-bottom: var(--s4);
  }
  .narrow {
    max-width: 7rem;
  }
  .name {
    font-weight: 500;
  }
  td .tag + .tag {
    margin-left: 4px;
  }
</style>
