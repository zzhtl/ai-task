<script lang="ts">
  /**
   * 任务列表。
   *
   * 每行要回答的是「这个任务什么时候会自己跑、上次跑得怎么样」——
   * 光列名字和 id 的列表，看完还得再点进去才知道有没有配定时。
   */
  import { goto } from '$app/navigation';
  import { listTasks, listRuns, triggerRun, newIdempotencyKey } from '$api/runs';
  import { api, describeError, ignoreForbidden } from '$api/client';
  import { resource } from '$api/resource.svelte';
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
  import Icon from '$lib/ui/Icon.svelte';

  let busy = $state<string | null>(null);
  let query = $state('');

  // 三个 key 全站共享：任务列表和命令面板、审批页是同一份。
  // 原来这里每 5 秒把 200 条完整 run 拉回来，只为在每一行里找"上次执行"。
  const tasksRes = resource<TaskSummary[]>(
    'tasks',
    (signal) => listTasks(signal).then((page) => page.items),
    { pollMs: 5000 }
  );
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
  const refresh = () => void tasksRes.refresh();

  const schedulesByTask = $derived.by(() => {
    const out = new Map<string, Schedule[]>();
    for (const s of schedules) (out.get(s.task_id) ?? out.set(s.task_id, []).get(s.task_id)!).push(s);
    return out;
  });

  // last_run 现在由 /api/v1/tasks 直接带回来（服务端一条 LATERAL）。
  // 之前这里每 5 秒拉 100 条 run 回来，只为在每一行里找"上次执行"。
  const lastRun = (taskId: string) => tasks.find((t) => t.id === taskId)?.last_run ?? undefined;
  const scheduleOf = (taskId: string) => schedulesByTask.get(taskId) ?? [];

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
  /**
   * 会被牵连的执行记录数。`null` 表示还在查。
   *
   * **打开确认框时才查。** 之前是常驻一份 100–200 条的 run 列表在内存里数，
   * 而这个数一分钟里用不上一次。接口不返回总数（ADR 0002：默认不返回总数），
   * 所以拉一页上限回来数；顶到上限就说"至少"，不编一个确切的数字。
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
      await refresh();
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }

  /**
   * 这次「想触发某个任务」的幂等键。
   *
   * 按任务存着，成功之后才丢掉：中途失败重试复用同一个键，
   * 服务端认得出这是同一次意图，不会建出第二个 run。
   */
  const triggerKeys = new Map<string, string>();

  async function run(task: TaskSummary) {
    busy = task.id;
    const key = triggerKeys.get(task.id) ?? newIdempotencyKey();
    triggerKeys.set(task.id, key);
    try {
      const r = await triggerRun(task.id, { dry_run: false }, key);
      triggerKeys.delete(task.id);
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
                  <Icon name="pencil" />
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
                  <Icon name="trash" />
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
    font-size: var(--t-sm);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sched {
    display: flex;
    gap: var(--s2);
    align-items: baseline;
    font-size: var(--t-sm);
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
</style>
