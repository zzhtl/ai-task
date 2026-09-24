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
  import { deleteRun, getRun, listAllTasks, listRuns, newIdempotencyKey, triggerRun } from '$api/runs';
  import { describeError } from '$api/client';
  import { pollWhileVisible, resource } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import { mergeFirstPage } from '$lib/runs/merge';
  import type { RunListItem } from '$api/types/RunListItem';
  import type { RunStatus } from '$api/types/RunStatus';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import type { TriggerKind } from '$api/types/TriggerKind';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, duration, money, stamp, triggerLabel, FAILED_STATUSES } from '$lib/ui/format';

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

  const isLive = (r: RunListItem) => r.status === 'running' || r.status === 'queued';
  // 停在审批门上的 run 状态还是 running。在列表里单独标出来：那是在等人，不是在跑。
  // 和侧栏徽标共享同一个 key，不多发请求
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });
  const awaitingRuns = $derived(new Set((approvals.data ?? []).map((a) => a.run_id)));
  // 有进行中的行时，耗时每秒走一次
  let now = $state(Date.now());
  const anyLive = $derived(runs.some(isLive));
  $effect(() => {
    if (!anyLive) return;
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const elapsed = (r: RunListItem) => {
    void now;
    return duration(r.started_at, r.finished_at);
  };

  function clearFilters() {
    status = 'all';
    trigger = 'all';
    range = 'all';
    taskId = '';
  }

  /**
   * 删执行记录。**连事件流和资源采样一起删，不可恢复。**
   * 只能删终态的：还在跑的 run，执行器仍在往它的事件流里写。
   */
  let pendingDelete = $state<RunListItem | null>(null);
  let busy = $state<string | null>(null);

  async function remove(run: RunListItem) {
    busy = run.id;
    try {
      await deleteRun(run.id);
      runs = runs.filter((r) => r.id !== run.id);
      toast('已删除这条执行记录');
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }

  /** 重跑：原样带上那一次的输入（列表里没有，先取一下详情）。 */
  async function rerun(run: RunListItem) {
    busy = run.id;
    try {
      const detail = await getRun(run.id);
      const r = await triggerRun(
        run.task_id,
        { dry_run: run.dry_run, inputs: detail.inputs ?? undefined },
        newIdempotencyKey()
      );
      toast(`已重新触发「${run.task_name}」`);
      await goto(`/runs/${r.id}`);
    } catch (e) {
      toastError(describeError(e));
      busy = null;
    }
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

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
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
    <p><b>{pendingDelete.task_name}</b> · {ago(pendingDelete.created_at)} · {money(pendingDelete.cost_usd)}</p>
    <p>连同它的完整事件流和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。</p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {/if}
</Confirm>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={6} /></div>
{:else if runs.length}
  <div class="card flush">
    <table>
      <thead>
        <tr>
          <th>状态</th>
          <th>任务</th>
          <th>触发</th>
          <th>开始</th>
          <th class="num">耗时</th>
          <th class="num">花费</th>
          <th class="act"></th>
        </tr>
      </thead>
      <tbody>
        {#each runs as r (r.id)}
          <tr class="clickable" onclick={() => goto(`/runs/${r.id}`)}>
            <td class="nowrap">
              <StatusBadge status={r.status === 'running' && awaitingRuns.has(r.id) ? 'awaiting_approval' : r.status} />
            </td>
            <td class="task-cell">
              <a href="/runs/{r.id}" onclick={(e) => e.stopPropagation()}>{r.task_name}</a>
              {#if r.dry_run}<span class="tag accent">影子</span>{/if}
              {#if r.error}
                <!-- 为了看一句「为什么失败」再点一次，是排查时最没必要的一次点击 -->
                <div class="err ellipsis" title={r.error}>{r.error}</div>
              {/if}
            </td>
            <td class="faint nowrap">{triggerLabel(r.trigger)}</td>
            <td class="faint nowrap" title={stamp(r.started_at ?? r.created_at)}>{ago(r.started_at ?? r.created_at)}</td>
            <td class="num" class:faint={!isLive(r)} class:live={isLive(r)}>{elapsed(r)}</td>
            <td class="num faint">{money(r.cost_usd)}</td>
            <td class="act">
              <div class="row">
                {#if canOperate && !isLive(r)}
                  <button
                    class="btn-ghost btn-sm btn-icon"
                    title="用同样的输入再跑一次"
                    aria-label="重跑"
                    disabled={busy === r.id}
                    onclick={(e) => {
                      e.stopPropagation();
                      void rerun(r);
                    }}
                  >
                    <Icon name="refresh" />
                  </button>
                {/if}
                <!-- 行本身是个链接，删除键不能顺带触发它 -->
                <button
                  class="btn-ghost btn-sm btn-icon danger"
                  title={!canOperate ? '需要 operator 权限' : isLive(r) ? '还在跑，先取消' : '删除这条执行记录'}
                  aria-label="删除执行记录"
                  disabled={isLive(r) || busy === r.id || !canOperate}
                  onclick={(e) => {
                    e.stopPropagation();
                    pendingDelete = r;
                  }}
                >
                  <Icon name="trash" />
                </button>
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
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
  .task-cell {
    max-width: 44ch;
  }
  .task-cell a {
    font-weight: 500;
  }
  .task-cell .tag {
    margin-left: var(--s1);
  }
  .live {
    color: var(--info-fg);
  }
  .more {
    display: flex;
    justify-content: center;
    padding: var(--s2);
    border-top: 1px solid var(--line);
  }
</style>
