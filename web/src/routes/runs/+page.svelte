<script lang="ts">
  /**
   * 执行记录。
   *
   * 这是排查时最常来的地方，所以：状态、任务、触发方式、时间范围都能筛，
   * 筛选写进 URL（链接能直接贴给同事："你看这几条失败"），失败原因**直接显示在列表里**
   * ——不该为了看一句「为什么失败」再点一次。
   *
   * 筛选全部在服务端做：拉最近一页再在浏览器里过滤，"失败 0"可能只是因为
   * 失败的那几条不在这一页里。
   */
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { listAllTasks, listRuns } from '$api/runs';
  import { describeError } from '$api/client';
  import { pollWhileVisible } from '$api/resource.svelte';
  import { session } from '$lib/auth/session.svelte';
  import { mergeFirstPage } from '$lib/runs/merge';
  import RunTable from '$lib/runs/RunTable.svelte';
  import type { RunListItem } from '$api/types/RunListItem';
  import type { RunStatus } from '$api/types/RunStatus';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import type { TriggerKind } from '$api/types/TriggerKind';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toastError } from '$lib/ui/toast.svelte';
  import { FAILED_STATUSES } from '$lib/ui/format';

  type StatusKey = 'all' | 'live' | 'failed' | 'succeeded' | 'cancelled';
  const STATUSES: Array<{ key: StatusKey; label: string; status: RunStatus[] | null }> = [
    { key: 'all', label: '全部', status: null },
    { key: 'live', label: '进行中', status: ['queued', 'running'] },
    { key: 'failed', label: '失败', status: FAILED_STATUSES as RunStatus[] },
    { key: 'succeeded', label: '成功', status: ['succeeded'] },
    { key: 'cancelled', label: '已取消', status: ['cancelled'] }
  ];
  type TriggerKey = 'all' | 'manual' | 'schedule';
  const TRIGGERS: Array<{ key: TriggerKey; label: string; trigger: TriggerKind[] | null }> = [
    { key: 'all', label: '全部', trigger: null },
    { key: 'manual', label: '手动', trigger: ['manual', 'api'] },
    { key: 'schedule', label: '定时', trigger: ['schedule'] }
  ];
  type RangeKey = 'all' | '24h' | '7d' | '30d';
  const RANGES: Array<{ key: RangeKey; label: string; hours: number | null }> = [
    { key: 'all', label: '全部时间', hours: null },
    { key: '24h', label: '最近 24 小时', hours: 24 },
    { key: '7d', label: '最近 7 天', hours: 24 * 7 },
    { key: '30d', label: '最近 30 天', hours: 24 * 30 }
  ];
  const PAGE = 50;

  const fromUrl = (key: string) => page.url.searchParams.get(key);
  let status = $state<StatusKey>((STATUSES.find((f) => f.key === fromUrl('status'))?.key ?? 'all') as StatusKey);
  let trigger = $state<TriggerKey>((TRIGGERS.find((f) => f.key === fromUrl('trigger'))?.key ?? 'all') as TriggerKey);
  let range = $state<RangeKey>((RANGES.find((f) => f.key === fromUrl('range'))?.key ?? 'all') as RangeKey);
  let taskId = $state<string>(fromUrl('task') ?? '');

  let runs = $state<RunListItem[]>([]);
  let tasks = $state<TaskSummary[]>([]);
  let nextCursor = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let loadingMore = $state(false);
  const canOperate = $derived(session.can('operator'));

  const filtered = $derived(status !== 'all' || trigger !== 'all' || range !== 'all' || taskId !== '');

  /** 当前筛选条件对应的查询。时间窗口在发请求那一刻算。 */
  function query(cursor: string | null = null) {
    const hours = RANGES.find((r) => r.key === range)?.hours ?? null;
    return {
      limit: PAGE,
      taskId: taskId || null,
      status: STATUSES.find((f) => f.key === status)?.status ?? null,
      trigger: TRIGGERS.find((f) => f.key === trigger)?.trigger ?? null,
      since: hours ? new Date(Date.now() - hours * 3600_000) : null,
      cursor
    };
  }

  /** 筛选变了就从第一页重来。 */
  async function reload() {
    loaded = false;
    try {
      const r = await listRuns(query());
      runs = r.items;
      nextCursor = r.next_cursor ?? null;
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  /**
   * 轮询只刷第一页，**合并**进已有列表：已经翻出来的后几页保留，
   * 已有的行按 id 更新状态，新出现的行插到最前。整表替换会把人翻到的位置冲掉。
   */
  async function poll() {
    if (!loaded) return;
    try {
      const r = await listRuns(query());
      runs = mergeFirstPage(runs, r.items);
      if (nextCursor === null) nextCursor = r.next_cursor ?? null;
      error = null;
    } catch (e) {
      error = describeError(e);
    }
  }

  async function more() {
    if (!nextCursor || loadingMore) return;
    loadingMore = true;
    try {
      const r = await listRuns(query(nextCursor));
      const known = new Set(runs.map((x) => x.id));
      runs = [...runs, ...r.items.filter((x) => !known.has(x.id))];
      nextCursor = r.next_cursor ?? null;
    } catch (e) {
      toastError(describeError(e));
    } finally {
      loadingMore = false;
    }
  }

  // 任务下拉只要取一次；任务名本身列表项里就带着
  $effect(() => {
    listAllTasks()
      .then((t) => (tasks = [...t].sort((a, b) => a.name.localeCompare(b.name))))
      .catch(() => {});
  });

  // 筛选写回 URL：这一页的链接是拿来贴给同事的
  $effect(() => {
    const params = new URLSearchParams();
    if (status !== 'all') params.set('status', status);
    if (trigger !== 'all') params.set('trigger', trigger);
    if (range !== 'all') params.set('range', range);
    if (taskId) params.set('task', taskId);
    const qs = params.toString();
    const target = qs ? `/runs?${qs}` : '/runs';
    // goto 之后 page.url 会变；不 untrack 的话这个 effect 会因此再跑一遍、多拉一次
    const current = untrack(() => page.url.pathname + page.url.search);
    if (current !== target) {
      void goto(target, { replaceState: true, noScroll: true, keepFocus: true });
    }
    void reload();
  });

  $effect(() => {
    // 这一页的轮询是**按 id 并进已翻出来的那几页**的，保住滚动位置和翻页结果，
    // 所以不走通用缓存（它是整份替换）。但看不见时照样该停。
    return pollWhileVisible(poll, 4000);
  });

  function clearFilters() {
    status = 'all';
    trigger = 'all';
    range = 'all';
    taskId = '';
  }

</script>

<PageHeader title="执行记录">
  {#snippet sub()}
    {#if loaded}<span>已加载 {runs.length} 条{nextCursor ? '，还有更早的' : ''}</span>{/if}
  {/snippet}
</PageHeader>

<div class="filters">
  <div class="seg" role="group" aria-label="状态">
    {#each STATUSES as f (f.key)}
      <button class:on={status === f.key} onclick={() => (status = f.key)}>{f.label}</button>
    {/each}
  </div>
  <div class="seg" role="group" aria-label="触发方式">
    {#each TRIGGERS as f (f.key)}
      <button class:on={trigger === f.key} onclick={() => (trigger = f.key)}>{f.label}</button>
    {/each}
  </div>
  <select bind:value={range} aria-label="时间范围">
    {#each RANGES as r (r.key)}<option value={r.key}>{r.label}</option>{/each}
  </select>
  <select bind:value={taskId} class="task-pick" aria-label="任务">
    <option value="">所有任务</option>
    {#each tasks as t (t.id)}<option value={t.id}>{t.name}</option>{/each}
  </select>
  {#if filtered}
    <button class="btn-ghost btn-sm" onclick={clearFilters}>清除筛选</button>
  {/if}
</div>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={6} /></div>
{:else if runs.length}
  <div class="card flush">
    <RunTable {runs} onremoved={(id) => (runs = runs.filter((r) => r.id !== id))} />
    {#if nextCursor}
      <div class="more">
        <button class="btn-ghost btn-sm" onclick={more} disabled={loadingMore}>
          {loadingMore ? '加载中…' : '加载更早的记录'}
        </button>
      </div>
    {/if}
  </div>
{:else}
  <Empty
    title={filtered ? '这个筛选下没有记录' : '还没有执行记录'}
    hint={filtered ? '换个条件看看。' : '建一个任务，手动触发或配上定时。'}
  >
    {#snippet action()}
      {#if filtered}
        <button class="btn-ghost" onclick={clearFilters}>清除筛选</button>
      {:else if canOperate}
        <a class="btn" href="/tasks/new">新建任务</a>
      {/if}
    {/snippet}
  </Empty>
{/if}

<style>
  .filters {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
    margin-bottom: var(--s4);
  }
  .task-pick {
    max-width: 16rem;
  }
  .more {
    display: flex;
    justify-content: center;
    padding: var(--s2);
    border-top: 1px solid var(--line);
  }
</style>
