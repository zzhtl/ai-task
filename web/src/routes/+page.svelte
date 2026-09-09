<script lang="ts">
  import { api, ApiFailure } from '$api/client';
  import { session, logout } from '$lib/auth/session.svelte';
  import { createTask, listRuns, listTasks, sampleTask, triggerRun } from '$api/runs';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskSummary } from '$api/types/TaskSummary';

  let tasks = $state<TaskSummary[]>([]);
  let runs = $state<RunSummary[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  async function refresh() {
    error = null;
    try {
      [tasks, runs] = await Promise.all([
        listTasks().then((p) => p.items),
        listRuns().then((p) => p.items)
      ]);
    } catch (e) {
      error = describe(e);
    }
  }

  async function act(fn: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await fn();
      await refresh();
    } catch (e) {
      error = describe(e);
    } finally {
      busy = false;
    }
  }

  function describe(e: unknown): string {
    return e instanceof ApiFailure ? `[${e.code}] ${e.message}` : String(e);
  }

  const newTask = () =>
    act(() => createTask(sampleTask(`统计行数-${new Date().toISOString().slice(11, 19)}`)));
  const run = (task: TaskSummary) => act(() => triggerRun(task.id));

  // 待审批数量做成角标：审批门挂着的时候 run 是停住的，
  // 这件事必须在首页就看得见，而不是等人想起来去翻。
  let pendingApprovals = $state(0);
  $effect(() => {
    const load = () =>
      api<{ items: unknown[] }>('/api/v1/approvals')
        .then((page) => (pendingApprovals = page.items.length))
        .catch(() => {});
    void load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });

  refresh();
</script>

<header>
  <div>
    <h1>ai-task</h1>
    <p class="sub">AI 执行控制平面</p>
  </div>
  <div class="tools">
    {#if session.identity}
      <span class="who muted">{session.identity.display_name}（{session.identity.role}）</span>
    {/if}
    {#if session.can('admin')}
      <a class="btn" href="/users">用户</a>
    {/if}
    <a class="btn" href="/approvals" class:urgent={pendingApprovals > 0}>
      待审批{#if pendingApprovals > 0}<span class="badge">{pendingApprovals}</span>{/if}
    </a>
    <button onclick={newTask} disabled={busy}>新建示例任务</button>
    {#if session.identity}
      <button onclick={() => logout().then(() => location.reload())}>退出</button>
    {/if}
  </div>
</header>

{#if error}<p class="bad">{error}</p>{/if}

<section>
  <h2>任务 <span class="muted">{tasks.length}</span></h2>
  {#if tasks.length === 0}
    <p class="muted">还没有任务。点右上角新建一个。</p>
  {:else}
    <ul class="list">
      {#each tasks as task (task.id)}
        <li>
          <div>
            <a href="/tasks/{task.id}"><strong>{task.name}</strong></a>
            {#if !task.enabled}<span class="tag">已停用</span>{/if}
            <div class="muted mono">{task.id}</div>
          </div>
          <button onclick={() => run(task)} disabled={busy || !task.enabled}>运行</button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<section>
  <h2>最近执行</h2>
  {#if runs.length === 0}
    <p class="muted">还没有执行记录。</p>
  {:else}
    <ul class="list">
      {#each runs as r (r.id)}
        <li>
          <div>
            <a href="/runs/{r.id}"><span class="status {r.status}">{r.status}</span></a>
            <span class="muted mono">{r.id.slice(0, 8)}</span>
            {#if r.dry_run}<span class="tag">影子</span>{/if}
            <div class="muted">
              {new Date(r.created_at).toLocaleString()} · ${r.cost_usd} · {r.max_seq} 事件
            </div>
          </div>
          <a class="btn" href="/runs/{r.id}">查看</a>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .tools { display: flex; gap: 0.5rem; align-items: center; }
  .who { font-size: 0.85rem; }
  .btn.urgent { border-color: var(--bad); color: var(--bad); }
  .badge {
    display: inline-block;
    min-width: 1.2rem;
    margin-left: 0.35rem;
    padding: 0 0.35rem;
    border-radius: 999px;
    background: var(--bad);
    color: #fff;
    font-size: 0.75rem;
    text-align: center;
  }

  header { display: flex; align-items: flex-start; justify-content: space-between; }
  h1 { margin: 0; font-size: 1.6rem; letter-spacing: -0.01em; }
  .sub { margin: 0.25rem 0 0; color: var(--muted); }
  h2 { font-size: 1rem; font-weight: 600; margin: 2rem 0 0.75rem; }
  .list { list-style: none; margin: 0; padding: 0; border: 1px solid var(--line); border-radius: 0.75rem; overflow: hidden; }
  .list li { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: 0.75rem 1rem; background: var(--card); }
  .list li + li { border-top: 1px solid var(--line); }
</style>
