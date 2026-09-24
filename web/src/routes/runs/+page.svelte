<script lang="ts">
  /**
   * 执行记录。
   *
   * 这是排查时最常来的地方，所以：状态可筛、任务可筛、行可扫、时间是相对的、
   * 失败原因**直接显示在列表里**——不该为了看一句「为什么失败」再点一次。
   *
   * 筛选在服务端做。以前是拉最近 100 条再在浏览器里过滤，翻不了页，
   * 而且"失败 0"可能只是因为失败的那几条不在这 100 条里。
   */
  import { session } from '$lib/auth/session.svelte';
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { deleteRun, listAllTasks, listRuns } from '$api/runs';
  import { describeError } from '$api/client';
  import { pollWhileVisible } from '$api/resource.svelte';
  import { mergeFirstPage } from '$lib/runs/merge';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { RunStatus } from '$api/types/RunStatus';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, duration, money, stamp, triggerLabel, FAILED_STATUSES } from '$lib/ui/format';
  import Icon from '$lib/ui/Icon.svelte';

  type FilterKey = 'all' | 'live' | 'failed' | 'succeeded';
  const FILTERS: Array<{ key: FilterKey; label: string; status: RunStatus[] | null }> = [
    { key: 'all', label: '全部', status: null },
    { key: 'live', label: '进行中', status: ['queued', 'running'] },
    { key: 'failed', label: '失败', status: FAILED_STATUSES as RunStatus[] },
    { key: 'succeeded', label: '成功', status: ['succeeded'] }
  ];
  const PAGE = 50;

  const fromUrl = (key: string) => page.url.searchParams.get(key);
  let filter = $state<FilterKey>(
    (FILTERS.find((f) => f.key === fromUrl('status'))?.key ?? 'all') as FilterKey
  );
  let taskId = $state<string>(fromUrl('task') ?? '');

  let runs = $state<RunSummary[]>([]);
  const canOperate = $derived(session.can('operator'));
  let tasks = $state<TaskSummary[]>([]);
  let nextCursor = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let loadingMore = $state(false);

  const statusOf = (key: FilterKey) => FILTERS.find((f) => f.key === key)?.status ?? null;

  /** 筛选变了就从第一页重来。 */
  async function reload() {
    loaded = false;
    try {
      const [r, t] = await Promise.all([
        listRuns({ limit: PAGE, taskId: taskId || null, status: statusOf(filter) }),
        listAllTasks()
      ]);
      runs = r.items;
      nextCursor = r.next_cursor ?? null;
      tasks = t;
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
      const r = await listRuns({ limit: PAGE, taskId: taskId || null, status: statusOf(filter) });
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
      const r = await listRuns({
        limit: PAGE,
        taskId: taskId || null,
        status: statusOf(filter),
        cursor: nextCursor
      });
      const known = new Set(runs.map((x) => x.id));
      runs = [...runs, ...r.items.filter((x) => !known.has(x.id))];
      nextCursor = r.next_cursor ?? null;
    } catch (e) {
      toastError(describeError(e));
    } finally {
      loadingMore = false;
    }
  }

  // 筛选写回 URL：这一页的链接是拿来贴给同事的（"你看这几条失败"）
  $effect(() => {
    const params = new URLSearchParams();
    if (filter !== 'all') params.set('status', filter);
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

  const taskName = (id: string) => tasks.find((t) => t.id === id)?.name ?? id.slice(0, 8);
  const isLive = (r: RunSummary) => r.status === 'running' || r.status === 'queued';

  /**
   * 删执行记录。**连事件流和资源采样一起删，不可恢复。**
   * 只能删终态的：还在跑的 run，执行器仍在往它的事件流里写。
   */
  let pendingDelete = $state<RunSummary | null>(null);
  let busy = $state<string | null>(null);

  async function remove(run: RunSummary) {
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
</script>

<PageHeader title="执行记录">
  {#snippet sub()}
    <span>
      已加载 {runs.length} 条{nextCursor ? '，还有更早的' : ''}
    </span>
  {/snippet}
  {#snippet actions()}
    <select bind:value={taskId} class="task-pick" title="只看某个任务">
      <option value="">所有任务</option>
      {#each tasks as t (t.id)}<option value={t.id}>{t.name}</option>{/each}
    </select>
    <div class="seg">
      {#each FILTERS as f (f.key)}
        <button class:on={filter === f.key} onclick={() => (filter = f.key)}>{f.label}</button>
      {/each}
    </div>
  {/snippet}
</PageHeader>

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
    <p>
      <b>{taskName(pendingDelete.task_id)}</b> · {ago(pendingDelete.created_at)} ·
      {money(pendingDelete.cost_usd)}
    </p>
    <p>连同它的完整事件流和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。</p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {/if}
</Confirm>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={5} /></div>
{:else if runs.length}
  <div class="card flush">
    <table>
      <thead>
        <tr>
          <th>状态</th>
          <th>任务</th>
          <th>触发</th>
          <th>耗时</th>
          <th>花费</th>
          <th>时间</th>
          <th>id</th>
          <th class="act"></th>
        </tr>
      </thead>
      <tbody>
        {#each runs as r (r.id)}
          <tr class="clickable" onclick={() => goto(`/runs/${r.id}`)}>
            <td class="nowrap"><StatusBadge status={r.status} /></td>
            <td class="task-cell">
              <a href="/runs/{r.id}" onclick={(e) => e.stopPropagation()}>{taskName(r.task_id)}</a>
              {#if r.dry_run}<span class="tag">影子</span>{/if}
              {#if r.error}
                <!-- 为了看一句「为什么失败」再点一次，是排查时最没必要的一次点击 -->
                <div class="err ellipsis" title={r.error}>{r.error}</div>
              {/if}
            </td>
            <td class="faint nowrap">{triggerLabel(r.trigger)}</td>
            <td class="faint nowrap">{duration(r.started_at, r.finished_at)}</td>
            <td class="faint nowrap">{money(r.cost_usd)}</td>
            <td class="faint nowrap" title={stamp(r.created_at)}>{ago(r.created_at)}</td>
            <td class="mono faint" title={r.id}>{r.id.slice(0, 8)}</td>
            <td class="act">
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
    title={filter === 'all' && !taskId ? '还没有执行记录' : '这个筛选下没有记录'}
    hint={filter === 'all' && !taskId ? '建一个任务，手动触发或配上定时。' : '换个状态或任务看看。'}
  >
    {#snippet action()}
      {#if filter === 'all' && !taskId}
        <a class="btn" href="/tasks/new">新建任务</a>
      {:else}
        <button
          class="btn-ghost"
          onclick={() => {
            filter = 'all';
            taskId = '';
          }}>清除筛选</button
        >
      {/if}
    {/snippet}
  </Empty>
{/if}

<style>
  .task-pick {
    max-width: 14rem;
  }
  .task-cell {
    max-width: 40ch;
  }
  .task-cell a {
    font-weight: 500;
  }
  .task-cell .tag {
    margin-left: var(--s1);
  }
  .more {
    display: flex;
    justify-content: center;
    padding: var(--s2);
    border-top: 1px solid var(--line);
  }
</style>
