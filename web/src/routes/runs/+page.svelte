<script lang="ts">
  /**
   * 执行记录。
   *
   * 这是排查时最常来的地方，所以：状态可筛、行可扫、时间是相对的、
   * 失败原因**直接显示在列表里**——不该为了看一句「为什么失败」再点一次。
   */
  import { listRuns, listTasks } from '$api/runs';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import { api } from '$api/client';
  import { ago, duration, money } from '$lib/ui/format';

  const FILTERS = [
    { key: 'all', label: '全部' },
    { key: 'live', label: '进行中' },
    { key: 'failed', label: '失败' },
    { key: 'succeeded', label: '成功' }
  ] as const;

  let filter = $state<(typeof FILTERS)[number]['key']>('all');
  let runs = $state<RunSummary[]>([]);
  let tasks = $state<TaskSummary[]>([]);
  let error = $state<string | null>(null);

  async function refresh() {
    try {
      const [r, t] = await Promise.all([listRuns(100), listTasks()]);
      runs = r.items;
      tasks = t.items;
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  $effect(() => {
    void refresh();
    const timer = setInterval(refresh, 4000);
    return () => clearInterval(timer);
  });

  const FAILED = ['failed', 'timed_out', 'budget_exceeded', 'resource_exceeded'];

  const shown = $derived(
    runs.filter((r) => {
      if (filter === 'live') return r.status === 'running' || r.status === 'queued';
      if (filter === 'failed') return FAILED.includes(r.status);
      if (filter === 'succeeded') return r.status === 'succeeded';
      return true;
    })
  );

  const taskName = (id: string) => tasks.find((t) => t.id === id)?.name ?? id.slice(0, 8);

  /**
   * 删执行记录。**连事件流和资源采样一起删，不可恢复。**
   *
   * 只能删终态的：还在跑的 run，执行器仍在往它的事件流里写。
   */
  let pendingDelete = $state<RunSummary | null>(null);
  let busy = $state<string | null>(null);

  const isLive = (r: RunSummary) => r.status === 'running' || r.status === 'queued';

  async function remove(run: RunSummary) {
    busy = run.id;
    try {
      await api(`/api/v1/runs/${run.id}`, { method: 'DELETE' });
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  function count(key: (typeof FILTERS)[number]['key']): number {
    if (key === 'live') return runs.filter((r) => ['running', 'queued'].includes(r.status)).length;
    if (key === 'failed') return runs.filter((r) => FAILED.includes(r.status)).length;
    if (key === 'succeeded') return runs.filter((r) => r.status === 'succeeded').length;
    return runs.length;
  }
</script>

<PageHeader title="执行记录">
  {#snippet actions()}
    <div class="filters">
      {#each FILTERS as f (f.key)}
        <button class:on={filter === f.key} class="btn-ghost btn-sm" onclick={() => (filter = f.key)}>
          {f.label}
          <span class="faint">{count(f.key)}</span>
        </button>
      {/each}
    </div>
  {/snippet}
</PageHeader>

<Confirm
  open={pendingDelete !== null}
  title="删除这次执行记录？"
  danger
  confirmText="删除"
  onconfirm={() => {
    const run = pendingDelete;
    pendingDelete = null;
    if (run) void remove(run);
  }}
>
  {#if pendingDelete}
    <p>
      <b>{taskName(pendingDelete.task_id)}</b> · {ago(pendingDelete.created_at)} ·
      {money(pendingDelete.cost_usd)}
    </p>
    <p>
      连同它的完整事件流和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。
    </p>
    <p class="warn-line">删掉之后拿不回来。</p>
  {/if}
</Confirm>

{#if error}<p class="bad">{error}</p>{/if}

{#if shown.length}
  <table>
    <thead>
      <tr>
        <th>状态</th><th>任务</th><th>触发</th><th>耗时</th><th>花费</th><th>时间</th><th>id</th><th></th>
      </tr>
    </thead>
    <tbody>
      {#each shown as r (r.id)}
        <tr onclick={() => (location.href = `/runs/${r.id}`)}>
          <td><StatusPill status={r.status} /></td>
          <td>
            <a href="/runs/{r.id}">{taskName(r.task_id)}</a>
            {#if r.dry_run}<span class="tag">影子</span>{/if}
            {#if r.error}
              <!-- 为了看一句「为什么失败」再点一次，是排查时最没必要的一次点击 -->
              <div class="err" title={r.error}>{r.error}</div>
            {/if}
          </td>
          <td class="faint">{r.trigger}</td>
          <td class="faint">{duration(r.started_at, r.finished_at)}</td>
          <td class="faint">{money(r.cost_usd)}</td>
          <td class="faint">{ago(r.created_at)}</td>
          <td class="mono faint">{r.id.slice(0, 8)}</td>
          <td class="act">
            <!-- 行本身是个链接，删除键不能顺带触发它 -->
            <button
              class="btn-ghost btn-sm danger"
              title={isLive(r) ? '还在跑，先取消' : '删除这条执行记录'}
              aria-label="删除执行记录"
              disabled={isLive(r) || busy === r.id}
              onclick={(e) => {
                e.stopPropagation();
                pendingDelete = r;
              }}>✕</button
            >
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{:else}
  <Empty
    title={filter === 'all' ? '还没有执行记录' : '这个筛选下没有记录'}
    hint={filter === 'all' ? '建一个任务，手动触发或配上定时。' : undefined}
  >
    {#snippet action()}
      {#if filter === 'all'}<a class="btn" href="/tasks/new">新建任务</a>{/if}
    {/snippet}
  </Empty>
{/if}

<style>
  .warn-line {
    color: var(--warn);
    margin-bottom: 0;
  }
  td.act {
    width: 1%;
    text-align: right;
  }
  td.act button.danger:hover:not(:disabled) {
    color: var(--bad);
  }
  .filters {
    display: flex;
    gap: 2px;
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r2);
    padding: 2px;
  }
  .filters button.on {
    background: var(--surface-3);
    color: var(--fg);
  }
  tbody tr {
    cursor: pointer;
  }
  .err {
    margin-top: 2px;
    font-size: 0.78rem;
    color: var(--bad);
    max-width: 46ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
