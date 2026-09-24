<script lang="ts">
  /**
   * 任务列表。
   *
   * 每行回答三件事：最近稳不稳（最近 10 次）、什么时候会自己跑（定时说人话）、
   * 要不要现在跑一下（行尾就能运行）。光列名字的列表，看完还得一个个点进去。
   */
  import { session } from '$lib/auth/session.svelte';
  import { goto } from '$app/navigation';
  import { listAllTasks, listRuns, newIdempotencyKey, setTaskEnabled, triggerRun } from '$api/runs';
  import { api, describeError, ignoreForbidden } from '$api/client';
  import { invalidate, resource } from '$api/resource.svelte';
  import { listSchedules, type Schedule } from '$api/models';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import SplitButton from '$lib/ui/SplitButton.svelte';
  import Dropdown from '$lib/ui/Dropdown.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import RunStrip from '$lib/tasks/RunStrip.svelte';
  import { describeCron } from '$lib/schedules/cron';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, stamp, until } from '$lib/ui/format';

  let busy = $state<string | null>(null);
  let query = $state('');
  /** viewer 只能看：写操作的按钮灰掉并说明原因，而不是点了再弹 403。 */
  const canOperate = $derived(session.can('operator'));

  // 两个 key 全站共享：任务列表和命令面板、执行记录页是同一份
  const tasksRes = resource<TaskSummary[]>('tasks', (signal) => listAllTasks(signal), { pollMs: 5000 });
  const schedulesRes = resource<Schedule[]>(
    'schedules',
    () =>
      listSchedules().catch((e) => {
        ignoreForbidden(e);
        return [] as Schedule[];
      }),
    { pollMs: 10_000 }
  );

  const tasks = $derived(tasksRes.data ?? []);
  const schedules = $derived(schedulesRes.data ?? []);
  const loaded = $derived(!tasksRes.pending);
  const error = $derived(tasksRes.error ? describeError(tasksRes.error) : null);

  const schedulesByTask = $derived.by(() => {
    const out = new Map<string, Schedule[]>();
    for (const s of schedules) {
      const list = out.get(s.task_id) ?? [];
      list.push(s);
      out.set(s.task_id, list);
    }
    // 启用的排前面：列表里只放得下一条，得是真的会跑的那条
    for (const list of out.values()) list.sort((a, b) => Number(b.enabled) - Number(a.enabled));
    return out;
  });
  const scheduleOf = (taskId: string) => schedulesByTask.get(taskId) ?? [];

  type Filter = 'all' | 'enabled' | 'disabled' | 'scheduled';
  let filter = $state<Filter>('all');
  const FILTERS: Array<{ id: Filter; label: string; test: (t: TaskSummary) => boolean }> = [
    { id: 'all', label: '全部', test: () => true },
    { id: 'enabled', label: '启用', test: (t) => t.enabled },
    { id: 'disabled', label: '停用', test: (t) => !t.enabled },
    { id: 'scheduled', label: '有定时', test: (t) => scheduleOf(t.id).some((s) => s.enabled) }
  ];
  const counts = $derived(Object.fromEntries(FILTERS.map((f) => [f.id, tasks.filter(f.test).length])));

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const pass = FILTERS.find((f) => f.id === filter)?.test ?? (() => true);
    return tasks.filter(
      (t) =>
        pass(t) &&
        (!q || t.name.toLowerCase().includes(q) || (t.description ?? '').toLowerCase().includes(q))
    );
  });

  /** 整行可点，但行里的按钮、链接、菜单要各干各的。 */
  function openRow(event: MouseEvent, id: string) {
    if ((event.target as HTMLElement).closest('a, button, input, [role="menu"]')) return;
    void goto(`/tasks/${id}`);
  }

  /**
   * 删任务。**级联删掉它的全部执行历史**——那些 run 里有成本记录和完整事件流，
   * 是审计材料，没有撤销键。所以先把会被牵连的数量摆出来。
   */
  let pendingDelete = $state<TaskSummary | null>(null);
  /**
   * 会被牵连的执行记录数。`null` 表示还在查。打开确认框时才查：接口不返回总数
   * （ADR 0002），拉一页上限回来数；顶到上限就说"至少"，不编一个确切的数字。
   */
  const RUN_PROBE_LIMIT = 200;
  let affectedRuns = $state<number | null>(null);
  let affectedAtLeast = $state(false);

  $effect(() => {
    const task = pendingDelete;
    if (!task) {
      affectedRuns = null;
      affectedAtLeast = false;
      return;
    }
    let cancelled = false;
    listRuns({ taskId: task.id, limit: RUN_PROBE_LIMIT })
      .then((page) => {
        if (cancelled) return;
        affectedRuns = page.items.length;
        affectedAtLeast = page.items.length >= RUN_PROBE_LIMIT;
      })
      .catch(() => {
        if (!cancelled) affectedRuns = null;
      });
    return () => {
      cancelled = true;
    };
  });

  async function remove(task: TaskSummary) {
    busy = task.id;
    try {
      await api(`/api/v1/tasks/${task.id}`, { method: 'DELETE' });
      toast(`已删除「${task.name}」`);
      invalidate('tasks', 'schedules');
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }

  async function toggle(task: TaskSummary) {
    busy = task.id;
    try {
      await setTaskEnabled(task.id, !task.enabled);
      toast(task.enabled ? `已停用「${task.name}」，定时不会再触发` : `已启用「${task.name}」`);
      invalidate('tasks');
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }

  /**
   * 这次「想触发某个任务」的幂等键。按任务 + 方式存着，成功之后才丢：
   * 中途失败重试复用同一个键，服务端认得出这是同一次意图，不会建出第二个 run。
   */
  const triggerKeys = new Map<string, string>();

  async function run(task: TaskSummary, dryRun: boolean) {
    busy = task.id;
    const slot = `${task.id}:${dryRun}`;
    const key = triggerKeys.get(slot) ?? newIdempotencyKey();
    triggerKeys.set(slot, key);
    try {
      const r = await triggerRun(task.id, { dry_run: dryRun }, key);
      triggerKeys.delete(slot);
      toast(dryRun ? `已触发「${task.name}」的影子执行` : `已触发「${task.name}」`);
      await goto(`/runs/${r.id}`);
    } catch (e) {
      toastError(describeError(e));
      busy = null;
    }
  }

  const runTitle = (task: TaskSummary) =>
    !canOperate ? '需要 operator 权限' : task.enabled ? '立即执行一次' : '任务已停用，先启用';
</script>

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
  title="删除任务「{pendingDelete?.name ?? ''}」？"
  danger
  confirmText="删除"
  onconfirm={() => {
    const task = pendingDelete;
    pendingDelete = null;
    if (task) void remove(task);
  }}
>
  {#if affectedRuns === null}
    <p class="muted">正在数会被牵连的执行记录…</p>
  {:else if affectedRuns > 0}
    <p>
      会同时删掉{#if affectedAtLeast}<b>至少 {affectedRuns}</b>{:else}<b>{affectedRuns}</b>{/if}
      次执行记录和它们的完整事件流。那里面有成本记录和策略判决，是审计材料。
    </p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {:else}
    <p>这个任务还没有执行记录。删掉之后拿不回来。</p>
  {/if}
</Confirm>

<PageHeader title="任务">
  {#snippet sub()}
    <span>{tasks.length} 个任务</span>
    <span>{schedules.filter((s) => s.enabled).length} 条定时在跑</span>
  {/snippet}
  {#snippet actions()}
    {#if canOperate}
      <a class="btn btn-primary" href="/tasks/new"><Icon name="plus" size={14} />新建任务</a>
    {/if}
  {/snippet}
</PageHeader>

<div class="filters">
  <div class="seg" role="group" aria-label="筛选">
    {#each FILTERS as f (f.id)}
      <button class:on={filter === f.id} onclick={() => (filter = f.id)}>
        {f.label}<span class="count">{counts[f.id] ?? 0}</span>
      </button>
    {/each}
  </div>
  <input class="search" bind:value={query} placeholder="按名称或描述找" type="search" aria-label="搜索任务" />
</div>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={4} /></div>
{:else if shown.length}
  <div class="card flush">
    <table>
      <thead>
        <tr>
          <th>任务</th>
          <th>最近 10 次</th>
          <th>上次执行</th>
          <th>定时</th>
          <th class="act"></th>
        </tr>
      </thead>
      <tbody>
        {#each shown as task (task.id)}
          {@const sched = scheduleOf(task.id)}
          {@const last = task.last_run}
          <tr class="clickable" class:off={!task.enabled} onclick={(e) => openRow(e, task.id)}>
            <td class="name-cell">
              <a class="name" href="/tasks/{task.id}">{task.name}</a>
              {#if !task.enabled}<span class="tag">已停用</span>{/if}
              {#if task.description}<div class="desc" title={task.description}>{task.description}</div>{/if}
            </td>
            <td>
              {#if task.recent_runs?.length}
                <RunStrip runs={task.recent_runs} />
              {:else}
                <span class="faint">还没跑过</span>
              {/if}
            </td>
            <td class="last-cell">
              {#if last}
                <div class="last">
                  <StatusBadge status={last.status} variant="text" />
                  {#if last.dry_run}<span class="tag accent">影子</span>{/if}
                  <span class="faint" title={stamp(last.finished_at ?? last.created_at)}>
                    {ago(last.finished_at ?? last.created_at)}
                  </span>
                </div>
                {#if last.error}<div class="err ellipsis" title={last.error}>{last.error}</div>{/if}
              {:else}
                <span class="faint">—</span>
              {/if}
            </td>
            <td>
              {#if sched.length}
                {@const s = sched[0]}
                <div class="sched" class:off={!s.enabled || !task.enabled}>
                  <span>{describeCron(s.cron)}</span>
                  {#if sched.length > 1}<span class="faint">等 {sched.length} 条</span>{/if}
                </div>
                <div class="faint small" title={s.next_three.join('\n')}>
                  {#if !task.enabled}任务停用，不会触发{:else if !s.enabled}已停用{:else if s.next_fire_at}下次 {until(s.next_fire_at)}{/if}
                </div>
              {:else}
                <span class="faint">只能手动触发</span>
              {/if}
            </td>
            <td class="act">
              <div class="row">
                <SplitButton
                  label="运行"
                  size="sm"
                  menuLabel="更多运行方式"
                  disabled={busy === task.id || !task.enabled || !canOperate}
                  title={runTitle(task)}
                  onclick={() => run(task, false)}
                >
                  <button onclick={() => run(task, true)}>
                    影子执行
                    <span class="hint">只记录意图不落地，用来试提示词</span>
                  </button>
                </SplitButton>
                {#if canOperate}
                  <a class="btn btn-ghost btn-sm btn-icon" href="/tasks/new?id={task.id}" title="编辑" aria-label="编辑">
                    <Icon name="pencil" />
                  </a>
                {/if}
                <Dropdown
                  label="更多操作"
                  triggerClass="btn-ghost btn-sm btn-icon"
                  disabled={busy === task.id || !canOperate}
                  title={canOperate ? '更多' : '需要 operator 权限'}
                >
                  {#snippet trigger()}<Icon name="more" />{/snippet}
                  <a href="/tasks/new?from={task.id}">
                    复制为新任务
                    <span class="hint">步骤和规则照搬，另起一个名字</span>
                  </a>
                  <button onclick={() => toggle(task)}>
                    {task.enabled ? '停用' : '启用'}
                    {#if task.enabled}<span class="hint">定时不再触发，也不能手动运行</span>{/if}
                  </button>
                  <hr />
                  <button class="danger" onclick={() => (pendingDelete = task)}>
                    删除
                    <span class="hint">连同全部执行记录</span>
                  </button>
                </Dropdown>
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if tasks.length}
  <Empty title="没有匹配的任务" hint="换个关键词或筛选试试。">
    {#snippet action()}
      <button
        class="btn-ghost"
        onclick={() => {
          filter = 'all';
          query = '';
        }}>清除筛选</button
      >
    {/snippet}
  </Empty>
{:else}
  <Empty title="还没有任务" hint="按顺序列出步骤：让 AI 做一件事、跑一条命令、停下来等人确认。">
    {#snippet action()}
      {#if canOperate}<a class="btn btn-primary" href="/tasks/new">新建任务</a>{/if}
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
  .filters .search {
    margin-left: auto;
    width: 16rem;
    max-width: 100%;
  }
  .count {
    margin-left: var(--s1);
    font-size: var(--t-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .name-cell {
    max-width: 36ch;
  }
  .name {
    font-weight: 500;
    color: var(--fg);
  }
  .name:hover {
    color: var(--accent-fg);
  }
  .name-cell .tag {
    margin-left: var(--s2);
  }
  .desc {
    margin-top: 2px;
    font-size: var(--t-sm);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .last-cell {
    max-width: 28ch;
  }
  .last {
    display: flex;
    gap: var(--s2);
    align-items: center;
    white-space: nowrap;
  }
  .sched {
    display: flex;
    gap: var(--s2);
    align-items: baseline;
    white-space: nowrap;
  }
  .sched.off {
    opacity: 0.55;
  }
  tr.off .name {
    color: var(--fg-dim);
  }
</style>
