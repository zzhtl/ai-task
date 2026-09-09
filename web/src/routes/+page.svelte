<script lang="ts">
  /**
   * 概览。
   *
   * 回答三个问题，按紧急程度排：**现在有什么在跑？有什么卡住等人？最近有什么坏了？**
   * 一个执行控制平面的首页不该是"任务列表"——那是配置视角，不是运行视角。
   */
  import { listRuns, listTasks } from '$api/runs';
  import { api } from '$api/client';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import { ago, money } from '$lib/ui/format';

  let runs = $state<RunSummary[]>([]);
  let tasks = $state<TaskSummary[]>([]);
  let approvals = $state<Array<{ id: string; run_id: string; title: string; expires_in_s: number }>>(
    []
  );
  let error = $state<string | null>(null);

  async function refresh() {
    try {
      const [r, t, a] = await Promise.all([
        listRuns(40),
        listTasks(),
        api<{ items: typeof approvals }>('/api/v1/approvals').catch(() => ({ items: [] }))
      ]);
      runs = r.items;
      tasks = t.items;
      approvals = a.items;
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  // 首页是"现在怎么样"，必须自己刷新。3 秒是人盯着屏幕时不会觉得卡顿的节奏。
  $effect(() => {
    void refresh();
    const timer = setInterval(refresh, 3000);
    return () => clearInterval(timer);
  });

  const live = $derived(runs.filter((r) => r.status === 'running' || r.status === 'queued'));
  const recent = $derived(runs.filter((r) => !live.includes(r)).slice(0, 12));
  const failed24h = $derived(
    runs.filter(
      (r) =>
        ['failed', 'timed_out', 'budget_exceeded', 'resource_exceeded'].includes(r.status) &&
        Date.now() - Date.parse(r.created_at) < 86_400_000
    )
  );
  const spend24h = $derived(
    runs
      .filter((r) => Date.now() - Date.parse(r.created_at) < 86_400_000)
      .reduce((sum, r) => sum + Number(r.cost_usd), 0)
  );

  const taskName = (id: string) => tasks.find((t) => t.id === id)?.name ?? id.slice(0, 8);
</script>

<PageHeader title="概览">
  {#snippet actions()}
    <a class="btn btn-primary" href="/tasks/new">新建任务</a>
  {/snippet}
</PageHeader>

{#if error}<p class="bad">{error}</p>{/if}

<section class="metrics">
  <div class="metric">
    <span class="k">正在执行</span>
    <strong class:live={live.length > 0}>{live.length}</strong>
  </div>
  <div class="metric">
    <span class="k">等待审批</span>
    <strong class:alert={approvals.length > 0}>{approvals.length}</strong>
  </div>
  <div class="metric">
    <span class="k">24h 失败</span>
    <strong class:alert={failed24h.length > 0}>{failed24h.length}</strong>
  </div>
  <div class="metric">
    <span class="k">24h 花费</span>
    <strong>${spend24h.toFixed(4)}</strong>
  </div>
  <div class="metric">
    <span class="k">任务</span>
    <strong>{tasks.length}</strong>
  </div>
</section>

{#if approvals.length}
  <section class="block urgent">
    <h2>等你点头</h2>
    <ul class="approvals">
      {#each approvals as a (a.id)}
        <li>
          <a href="/runs/{a.run_id}">
            <span class="dot running"></span>
            <span class="title">{a.title}</span>
            <span class="spacer"></span>
            <span class="muted">剩余 {Math.floor(a.expires_in_s / 60)}:{String(a.expires_in_s % 60).padStart(2, '0')}</span>
          </a>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<div class="cols">
  <section class="block">
    <h2>正在执行 <span class="faint">{live.length}</span></h2>
    {#if live.length}
      <ul class="runs">
        {#each live as r (r.id)}
          <li>
            <a href="/runs/{r.id}">
              <StatusPill status={r.status} />
              <span class="task">{taskName(r.task_id)}</span>
              <span class="spacer"></span>
              <span class="mono faint">{r.id.slice(0, 8)}</span>
              <span class="faint">{ago(r.started_at ?? r.created_at)}</span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty title="现在没有东西在跑" hint="定时任务到点会自己启动，也可以手动触发。" />
    {/if}
  </section>

  <section class="block">
    <h2>最近结束</h2>
    {#if recent.length}
      <ul class="runs">
        {#each recent as r (r.id)}
          <li>
            <a href="/runs/{r.id}">
              <StatusPill status={r.status} />
              <span class="task">{taskName(r.task_id)}</span>
              <span class="spacer"></span>
              <span class="faint">{money(r.cost_usd)}</span>
              <span class="faint">{ago(r.finished_at ?? r.created_at)}</span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty title="还没有执行记录">
        {#snippet action()}
          <a class="btn" href="/tasks/new">建一个任务</a>
        {/snippet}
      </Empty>
    {/if}
  </section>
</div>

<style>
  .metrics {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: var(--s3);
    margin-bottom: var(--s5);
  }
  .metric {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s3) var(--s4);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .k {
    font-size: 0.75rem;
    color: var(--fg-faint);
  }
  .metric strong {
    font-size: 1.6rem;
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  .metric strong.live {
    color: var(--st-running);
  }
  .metric strong.alert {
    color: var(--bad);
  }

  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s4);
  }
  @media (max-width: 1000px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }

  .block {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s4);
  }
  .block.urgent {
    border-color: color-mix(in srgb, var(--bad) 35%, var(--line));
    margin-bottom: var(--s4);
  }
  h2 {
    margin-bottom: var(--s3);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  ul a {
    display: flex;
    align-items: center;
    gap: var(--s3);
    padding: 0.45rem var(--s2);
    border-radius: var(--r2);
    font-size: 0.85rem;
    transition: background 0.1s ease;
  }
  ul a:hover {
    background: var(--surface-2);
    color: var(--fg);
  }
  .task {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .approvals .title {
    color: var(--fg);
  }
</style>
