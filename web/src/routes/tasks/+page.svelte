<script lang="ts">
  /**
   * 任务列表。
   *
   * 每行要回答的是「这个任务什么时候会自己跑、上次跑得怎么样」——
   * 光列名字和 id 的列表，看完还得再点进去才知道有没有配定时。
   */
  import { listTasks, listRuns, triggerRun } from '$api/runs';
  import { api } from '$api/client';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import type { RunSummary } from '$api/types/RunSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import { ago } from '$lib/ui/format';

  interface Schedule {
    id: string;
    task_id: string;
    cron: string;
    timezone: string;
    enabled: boolean;
    next_three: string[];
  }

  let tasks = $state<TaskSummary[]>([]);
  let runs = $state<RunSummary[]>([]);
  let schedules = $state<Schedule[]>([]);
  let error = $state<string | null>(null);
  let busy = $state<string | null>(null);

  async function refresh() {
    try {
      const [t, r, s] = await Promise.all([
        listTasks(),
        listRuns(100),
        api<{ items: Schedule[] }>('/api/v1/schedules').catch(() => ({ items: [] }))
      ]);
      tasks = t.items;
      runs = r.items;
      schedules = s.items;
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  $effect(() => {
    void refresh();
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  });

  const lastRun = (taskId: string) => runs.find((r) => r.task_id === taskId);
  const scheduleOf = (taskId: string) => schedules.filter((s) => s.task_id === taskId);

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
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }

  async function run(task: TaskSummary) {
    busy = task.id;
    try {
      const r = await triggerRun(task.id);
      location.href = `/runs/${r.id}`;
    } catch (e) {
      error = String(e);
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
    <p class="warn-line">删掉之后拿不回来。</p>
  {:else}
    <p>这个任务还没有执行记录。删掉之后拿不回来。</p>
  {/if}
</Confirm>

<PageHeader title="任务">
  {#snippet actions()}
    <a class="btn btn-primary" href="/tasks/new">新建任务</a>
  {/snippet}
</PageHeader>

{#if error}<p class="bad">{error}</p>{/if}

{#if tasks.length}
  <div class="grid">
    {#each tasks as task (task.id)}
      {@const sched = scheduleOf(task.id)}
      {@const last = lastRun(task.id)}
      <article class="card">
        <header>
          <a href="/tasks/{task.id}" class="name">{task.name}</a>
          <span class="spacer"></span>
          <button class="btn-sm" disabled={busy === task.id} onclick={() => run(task)}>运行</button>
          <button
            class="btn-ghost btn-sm danger"
            disabled={busy === task.id}
            title="删除任务"
            aria-label="删除任务"
            onclick={() => (pendingDelete = task)}>✕</button
          >
        </header>

        {#if task.description}<p class="desc">{task.description}</p>{/if}

        <div class="facts">
          {#if sched.length}
            {#each sched as s (s.id)}
              <div class="fact" class:off={!s.enabled}>
                <code>{s.cron}</code>
                <span class="faint">
                  {s.enabled ? (s.next_three[0] ?? '算不出触发点') : '已停用'}
                </span>
              </div>
            {/each}
          {:else}
            <div class="fact faint">未配定时，只能手动触发</div>
          {/if}

          <div class="fact">
            {#if last}
              <StatusPill status={last.status} />
              <span class="faint">{ago(last.finished_at ?? last.created_at)}</span>
            {:else}
              <span class="faint">还没跑过</span>
            {/if}
          </div>
        </div>

        <footer class="faint">
          <span>v{task.version}</span>
          <span class="mono">{task.id.slice(0, 8)}</span>
          {#if !task.enabled}<span class="tag danger">已停用</span>{/if}
        </footer>
      </article>
    {/each}
  </div>
{:else}
  <Empty title="还没有任务" hint="按顺序列出步骤：让 AI 做一件事、跑一条命令、停下来等人确认。每一步都能指定在哪台机器上跑。">
    {#snippet action()}
      <a class="btn btn-primary" href="/tasks/new">新建任务</a>
    {/snippet}
  </Empty>
{/if}

<style>
  .warn-line {
    color: var(--warn);
    margin-bottom: 0;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: var(--s3);
  }
  article {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    transition: border-color 0.12s ease;
  }
  article:hover {
    border-color: var(--line-strong);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }
  header button.danger:hover:not(:disabled) { color: var(--bad); }
  .name {
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .desc {
    margin: 0;
    font-size: 0.82rem;
    color: var(--fg-dim);
  }
  .facts {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding: var(--s2) 0;
    border-top: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
  }
  .fact {
    display: flex;
    align-items: center;
    gap: var(--s2);
    font-size: 0.8rem;
  }
  .fact.off {
    opacity: 0.5;
  }
  footer {
    display: flex;
    gap: var(--s3);
    font-size: 0.75rem;
    align-items: center;
  }
</style>
