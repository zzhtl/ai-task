<script lang="ts">
  /**
   * 任务列表。
   *
   * 每行要回答的是「这个任务什么时候会自己跑、上次跑得怎么样」——
   * 光列名字和 id 的列表，看完还得再点进去才知道有没有配定时。
   */
  import { goto } from '$app/navigation';
  import { listTasks, listRuns, triggerRun } from '$api/runs';
  import { api, describeError } from '$api/client';
  import { listSchedules, type Schedule } from '$api/models';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import type { RunSummary } from '$api/types/RunSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, stamp } from '$lib/ui/format';

  let tasks = $state<TaskSummary[]>([]);
  let runs = $state<RunSummary[]>([]);
  let schedules = $state<Schedule[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let busy = $state<string | null>(null);
  let query = $state('');

  async function refresh() {
    try {
      const [t, r, s] = await Promise.all([
        listTasks(),
        listRuns(200),
        listSchedules().catch(() => [] as Schedule[])
      ]);
      tasks = t.items;
      runs = r.items;
      schedules = s;
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  $effect(() => {
    void refresh();
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  });

  const lastRun = (taskId: string) => runs.find((r) => r.task_id === taskId);
  const scheduleOf = (taskId: string) => schedules.filter((s) => s.task_id === taskId);

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return tasks;
    return tasks.filter(
      (t) => t.name.toLowerCase().includes(q) || (t.description ?? '').toLowerCase().includes(q)
    );
  });

  /**
   * 删任务。**级联删掉它的全部执行历史**——那些 run 里有成本记录和完整事件流，
   * 是审计材料，没有撤销键。所以先把会被牵连的数量摆出来。
   */
  let pendingDelete = $state<TaskSummary | null>(null);
  const affectedRuns = $derived.by(() => {
    const task = pendingDelete;
    return task ? runs.filter((r) => r.task_id === task.id).length : 0;
  });

  async function remove(task: TaskSummary) {
    busy = task.id;
    try {
      await api(`/api/v1/tasks/${task.id}`, { method: 'DELETE' });
      toast(`已删除「${task.name}」`);
      await refresh();
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }

  async function run(task: TaskSummary) {
    busy = task.id;
    try {
      const r = await triggerRun(task.id);
      toast(`已触发「${task.name}」`);
      await goto(`/runs/${r.id}`);
    } catch (e) {
      toastError(describeError(e));
      busy = null;
    }
  }
</script>

<Confirm
  open={pendingDelete !== null}
  title="删除任务「{pendingDelete?.name ?? ''}」？"
  danger
  confirmText="删除"
  onconfirm={() => {
    const task = pendingDelete;
    pendingDelete = null;
    if (task) void remove(task);
  }}
>
  {#if affectedRuns}
    <p>
      会同时删掉 <b>{affectedRuns}</b> 次执行记录和它们的完整事件流。
      那里面有成本记录和策略判决，是审计材料。
    </p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {:else}
    <p>这个任务还没有执行记录。删掉之后拿不回来。</p>
  {/if}
</Confirm>

<PageHeader title="任务">
  {#snippet sub()}
    <span>{tasks.length} 个任务，{schedules.filter((s) => s.enabled).length} 条定时在跑</span>
  {/snippet}
  {#snippet actions()}
    <input class="search" bind:value={query} placeholder="按名称筛选" type="search" />
    <a class="btn btn-primary" href="/tasks/new">新建任务</a>
  {/snippet}
</PageHeader>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={4} /></div>
{:else if shown.length}
  <div class="card flush">
    <table>
      <thead>
        <tr>
          <th>任务</th>
          <th>定时</th>
          <th>上次执行</th>
          <th>版本</th>
          <th class="act"></th>
        </tr>
      </thead>
      <tbody>
        {#each shown as task (task.id)}
          {@const sched = scheduleOf(task.id)}
          {@const last = lastRun(task.id)}
          <tr class="clickable" class:off={!task.enabled} onclick={() => goto(`/tasks/${task.id}`)}>
            <td class="name-cell">
              <a class="name" href="/tasks/{task.id}" onclick={(e) => e.stopPropagation()}>
                {task.name}
              </a>
              {#if !task.enabled}<span class="tag danger">已停用</span>{/if}
              {#if task.description}<div class="desc">{task.description}</div>{/if}
            </td>
            <td>
              {#if sched.length}
                {#each sched.slice(0, 2) as s (s.id)}
                  <div class="sched" class:off={!s.enabled}>
                    <code>{s.cron}</code>
                    <span class="faint" title={s.next_three.join('\n')}>
                      {s.enabled ? (s.next_three[0] ?? '算不出触发点') : '已停用'}
                    </span>
                  </div>
                {/each}
                {#if sched.length > 2}<span class="faint">还有 {sched.length - 2} 条</span>{/if}
              {:else}
                <span class="faint">只能手动触发</span>
              {/if}
            </td>
            <td>
              {#if last}
                <div class="last">
                  <StatusPill status={last.status} />
                  <span class="faint" title={stamp(last.finished_at ?? last.created_at)}>
                    {ago(last.finished_at ?? last.created_at)}
                  </span>
                </div>
                {#if last.error}<div class="err ellipsis" title={last.error}>{last.error}</div>{/if}
              {:else}
                <span class="faint">还没跑过</span>
              {/if}
            </td>
            <td class="faint">
              v{task.version}
              <span class="mono" title={task.id}>· {task.id.slice(0, 8)}</span>
            </td>
            <td class="act">
              <div class="row">
                <button
                  class="btn-sm"
                  disabled={busy === task.id || !task.enabled}
                  title={task.enabled ? '立即执行一次' : '任务已停用，先在详情页启用'}
                  onclick={(e) => {
                    e.stopPropagation();
                    void run(task);
                  }}>运行</button
                >
                <a
                  class="btn btn-ghost btn-sm btn-icon"
                  href="/tasks/new?id={task.id}"
                  title="编辑"
                  aria-label="编辑"
                  onclick={(e) => e.stopPropagation()}
                >
                  <svg viewBox="0 0 24 24"><path d="M4 20h4l10-10-4-4L4 16v4zM13 7l4 4" /></svg>
                </a>
                <button
                  class="btn-ghost btn-sm btn-icon danger"
                  disabled={busy === task.id}
                  title="删除任务"
                  aria-label="删除任务"
                  onclick={(e) => {
                    e.stopPropagation();
                    pendingDelete = task;
                  }}
                >
                  <svg viewBox="0 0 24 24"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" /></svg>
                </button>
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if tasks.length}
  <Empty title="没有匹配的任务" hint="换个关键词试试。" />
{:else}
  <Empty
    title="还没有任务"
    hint="按顺序列出步骤：让 AI 做一件事、跑一条命令、停下来等人确认。每一步都能指定在哪台机器上跑。"
  >
    {#snippet action()}
      <a class="btn btn-primary" href="/tasks/new">新建任务</a>
    {/snippet}
  </Empty>
{/if}

<style>
  .search {
    width: 14rem;
  }
  .name-cell {
    max-width: 34ch;
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
    font-size: 0.78rem;
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sched {
    display: flex;
    gap: var(--s2);
    align-items: baseline;
    font-size: 0.8rem;
    white-space: nowrap;
  }
  .sched.off {
    opacity: 0.5;
  }
  .last {
    display: flex;
    gap: var(--s2);
    align-items: center;
    white-space: nowrap;
  }
  .err {
    margin-top: 2px;
    font-size: 0.74rem;
    color: var(--bad);
    max-width: 36ch;
  }
</style>
