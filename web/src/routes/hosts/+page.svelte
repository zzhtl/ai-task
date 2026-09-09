<script lang="ts">
  /**
   * SSH 主机。任务要下发到别的机器，先在这里加一台。
   *
   * 私钥只进不出：加密后落库，任何读接口都不返回它。这个页面也永远显示不出来。
   */
  import { api, ApiFailure } from '$api/client';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import { ago } from '$lib/ui/format';

  interface Host {
    id: string;
    name: string;
    address: string;
    port: number;
    username: string;
    tags: string[];
    agent_version: string | null;
    ai_clis: Array<{ name: string; path: string; version: string | null }>;
    cgroup_mode: string | null;
    degraded: boolean;
    last_seen_at: string | null;
  }

  let hosts = $state<Host[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  let name = $state('');
  let address = $state('');
  let port = $state(22);
  let username = $state('');
  let tags = $state('');
  let privateKey = $state('');

  async function load() {
    try {
      hosts = (await api<{ items: Host[] }>('/api/v1/hosts')).items;
      error = null;
    } catch (e) {
      error = e instanceof ApiFailure ? e.message : String(e);
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
          tags: tags.split(',').map((t) => t.trim()).filter(Boolean),
          private_key: privateKey
        }
      });
      name = '';
      address = '';
      username = '';
      tags = '';
      privateKey = '';
      await load();
    } catch (e) {
      error = e instanceof ApiFailure ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    void load();
  });
</script>

<PageHeader title="主机">
  {#snippet sub()}
    <span>任务要下发到别的机器，先在这里加一台。私钥加密落库，任何读接口都不返回。</span>
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <p class="bad">需要管理员权限：这里存的是 SSH 私钥。</p>
{:else}
  {#if error}<p class="bad">{error}</p>{/if}

  <table>
    <thead>
      <tr><th>名称</th><th>地址</th><th>用户</th><th>tag</th><th>可直接跑的 AI</th><th>资源归因</th><th>最近连接</th></tr>
    </thead>
    <tbody>
      {#each hosts as host (host.id)}
        <tr>
          <td>{host.name}</td>
          <td class="mono">{host.address}:{host.port}</td>
          <td class="mono">{host.username}</td>
          <td>{host.tags.join(', ') || '—'}</td>
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
              <span class="tag danger">{host.cgroup_mode} · 上限未强制</span>
            {:else}
              <span class="tag ok">{host.cgroup_mode}</span>
            {/if}
          </td>
          <td class="faint">{ago(host.last_seen_at)}</td>
        </tr>
      {/each}
    </tbody>
  </table>

  {#if hosts.length === 0}
    <Empty title="还没有主机" hint="加一台之后，任务节点上写 host.host_id 就能把命令下发过去。" />
  {/if}

  <section class="new card">
    <h2>加一台</h2>
    <div class="row">
      <input bind:value={name} placeholder="名称，如 prod-web-1" />
      <input bind:value={address} placeholder="地址或 IP" />
      <input bind:value={port} type="number" min="1" max="65535" class="port" />
      <input bind:value={username} placeholder="登录用户名" />
      <input bind:value={tags} placeholder="tag，逗号分隔（策略按 tag 生效）" />
    </div>
    <textarea
      bind:value={privateKey}
      spellcheck="false"
      placeholder="OpenSSH 私钥内容，-----BEGIN ... PRIVATE KEY----- 开头"
    ></textarea>
    <p class="muted">
      私钥加密后落库，任何读接口都不返回它。首次连接会记下对方的主机密钥（TOFU）；
      <strong>密钥变了永远是拒绝</strong>——那是中间人攻击的信号。
    </p>
    <button class="btn-primary" onclick={create} disabled={busy || !name || !address || !username || !privateKey}>
      添加主机
    </button>
  </section>
{/if}

<style>
  .new { margin-top: var(--s5); display: flex; flex-direction: column; gap: var(--s3); }
  .row { display: flex; gap: var(--s2); flex-wrap: wrap; }
  .row input { flex: 1; min-width: 9rem; }
  .port { flex: 0 0 5rem !important; min-width: 5rem !important; }
  textarea { width: 100%; min-height: 7rem; }
  .new button { align-self: flex-start; }
</style>
