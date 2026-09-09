<script lang="ts">
  /**
   * 概览。
   *
   * 回答四个问题，按紧急程度排：**有什么卡住等人？现在有什么在跑？最近有什么坏了？
   * 接下来什么时候会自己跑？**一个执行控制平面的首页不该是"任务列表"——
   * 那是配置视角，不是运行视角。
   */
  import { listRuns, listTasks } from '$api/runs';
  import { listApprovals, listSchedules, type Approval, type Schedule } from '$api/models';
  import { describeError } from '$api/client';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { ago, money, mmss, stamp, triggerLabel, FAILED_STATUSES } from '$lib/ui/format';

  let runs = $state<RunSummary[]>([]);
  let tasks = $state<TaskSummary[]>([]);
  let approvals = $state<Approval[]>([]);
  let schedules = $state<Schedule[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);

  async function refresh() {
    try {
      const [r, t, a, s] = await Promise.all([
        listRuns(200),
        listTasks(),
        listApprovals().catch(() => [] as Approval[]),
        listSchedules().catch(() => [] as Schedule[])
      ]);
      runs = r.items;
      tasks = t.items;
      approvals = a;
      schedules = s;
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  // 首页是"现在怎么样"，必须自己刷新。3 秒是人盯着屏幕时不会觉得卡顿的节奏。
  $effect(() => {
    void refresh();
    const timer = setInterval(refresh, 3000);
    return () => clearInterval(timer);
  });

  const DAY = 86_400_000;
  const live = $derived(runs.filter((r) => r.status === 'running' || r.status === 'queued'));
  const recent = $derived(runs.filter((r) => !live.includes(r)).slice(0, 10));
  const failed24h = $derived(
    runs.filter(
      (r) => FAILED_STATUSES.includes(r.status) && Date.now() - Date.parse(r.created_at) < DAY
    )
  );
  const spend24h = $derived(
    runs
      .filter((r) => Date.now() - Date.parse(r.created_at) < DAY)
      .reduce((sum, r) => sum + Number(r.cost_usd), 0)
  );
  const runs24h = $derived(runs.filter((r) => Date.now() - Date.parse(r.created_at) < DAY).length);

  /** 接下来要自己响的定时，按最近的排。停用的任务不会响，过滤掉。 */
  const upcoming = $derived(
    schedules
      .filter((s) => s.enabled && s.next_fire_at && tasks.find((t) => t.id === s.task_id)?.enabled)
      .sort((a, b) => Date.parse(a.next_fire_at!) - Date.parse(b.next_fire_at!))
      .slice(0, 6)
  );

  const taskName = (id: string) => tasks.find((t) => t.id === id)?.name ?? id.slice(0, 8);

  /** 到某个时刻还有多久。定时的"下次"看相对值比看绝对时间快。 */
  function until(iso: string): string {
    const ms = Date.parse(iso) - Date.now();
    if (ms <= 0) return '马上';
    const m = Math.round(ms / 60_000);
    if (m < 60) return `${m} 分钟后`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h} 小时 ${m % 60} 分后`;
    return `${Math.floor(h / 24)} 天后`;
  }
</script>

<PageHeader title="概览">
  {#snippet actions()}
    <a class="btn btn-primary" href="/tasks/new">新建任务</a>
  {/snippet}
</PageHeader>

{#if error}<div class="banner">{error}</div>{/if}

<section class="metrics">
  <a class="metric" class:live={live.length > 0} href="/runs?status=live">
    <span class="k">正在执行</span>
    <strong>{live.length}</strong>
    <span class="hint">{live.length ? '点开看进度' : '空闲'}</span>
  </a>
  <a class="metric" class:alert={approvals.length > 0} href="/approvals">
    <span class="k">等待审批</span>
    <strong>{approvals.length}</strong>
    <span class="hint">{approvals.length ? '超时会按拒绝处理' : '没有卡住的'}</span>
  </a>
  <a class="metric" class:alert={failed24h.length > 0} href="/runs?status=failed">
    <span class="k">24h 失败</span>
    <strong>{failed24h.length}</strong>
    <span class="hint">共 {runs24h} 次执行</span>
  </a>
  <div class="metric">
    <span class="k">24h 花费</span>
    <strong>{money(spend24h)}</strong>
    <span class="hint">按 run 累计的模型费用</span>
  </div>
  <a class="metric" href="/tasks">
    <span class="k">任务</span>
    <strong>{tasks.filter((t) => t.enabled).length}</strong>
    <span class="hint">
      {tasks.length - tasks.filter((t) => t.enabled).length
        ? `另有 ${tasks.length - tasks.filter((t) => t.enabled).length} 个已停用`
        : `${schedules.filter((s) => s.enabled).length} 条定时在跑`}
    </span>
  </a>
</section>

{#if approvals.length}
  <section class="card urgent">
    <header class="card-head">
      <h2>等你点头</h2>
      <span class="sub">超时未决一律按拒绝处理</span>
      <span class="spacer"></span>
      <a class="btn btn-sm" href="/approvals">去审批</a>
    </header>
    <ul class="list">
      {#each approvals as a (a.id)}
        <li>
          <a href="/runs/{a.run_id}">
            <span class="dot awaiting_approval"></span>
            <span class="main ellipsis">{a.title}</span>
            <span class="spacer"></span>
            <span class="faint">{taskName(runs.find((r) => r.id === a.run_id)?.task_id ?? '')}</span>
            <span class="clock" class:soon={a.expires_in_s < 60}>剩余 {mmss(a.expires_in_s)}</span>
          </a>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<div class="cols">
  <section class="card">
    <header class="card-head">
      <h2>正在执行</h2>
      <span class="sub">{live.length} 个</span>
      <span class="spacer"></span>
      <a class="btn btn-ghost btn-sm" href="/runs?status=live">全部</a>
    </header>
    {#if !loaded}
      <Loading rows={3} />
    {:else if live.length}
      <ul class="list">
        {#each live as r (r.id)}
          <li>
            <a href="/runs/{r.id}">
              <StatusPill status={r.status} />
              <span class="main ellipsis">{taskName(r.task_id)}</span>
              {#if r.dry_run}<span class="tag">影子</span>{/if}
              <span class="spacer"></span>
              <span class="faint">{triggerLabel(r.trigger)}</span>
              <span class="faint" title={stamp(r.started_at ?? r.created_at)}>
                {ago(r.started_at ?? r.created_at)}
              </span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty compact title="现在没有东西在跑" hint="定时任务到点会自己启动，也可以在任务页手动触发。" />
    {/if}
  </section>

  <section class="card">
    <header class="card-head">
      <h2>最近结束</h2>
      <span class="spacer"></span>
      <a class="btn btn-ghost btn-sm" href="/runs">全部</a>
    </header>
    {#if !loaded}
      <Loading rows={4} />
    {:else if recent.length}
      <ul class="list">
        {#each recent as r (r.id)}
          <li>
            <a href="/runs/{r.id}">
              <StatusPill status={r.status} />
              <span class="main">
                <span class="ellipsis">{taskName(r.task_id)}</span>
                {#if r.error}<span class="err ellipsis" title={r.error}>{r.error}</span>{/if}
              </span>
              <span class="spacer"></span>
              <span class="faint">{money(r.cost_usd)}</span>
              <span class="faint" title={stamp(r.finished_at ?? r.created_at)}>
                {ago(r.finished_at ?? r.created_at)}
              </span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty compact title="还没有执行记录" hint="建一个任务，手动跑一次试试。">
        {#snippet action()}
          <a class="btn" href="/tasks/new">建一个任务</a>
        {/snippet}
      </Empty>
    {/if}
  </section>

  <section class="card wide">
    <header class="card-head">
      <h2>接下来会触发</h2>
      <span class="sub">只列启用中的定时，按时间先后</span>
    </header>
    {#if !loaded}
      <Loading rows={2} />
    {:else if upcoming.length}
      <ul class="list">
        {#each upcoming as s (s.id)}
          <li>
            <a href="/tasks/{s.task_id}">
              <span class="dot ready"></span>
              <span class="main ellipsis">{taskName(s.task_id)}</span>
              <code class="cron">{s.cron}</code>
              <span class="faint">{s.timezone}</span>
              <span class="spacer"></span>
              <span class="faint" title={s.next_three.join('\n')}>{s.next_three[0] ?? stamp(s.next_fire_at)}</span>
              <span class="next">{until(s.next_fire_at!)}</span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty compact title="没有启用中的定时" hint="在任务详情页给任务配上 cron，它就会出现在这里。" />
    {/if}
  </section>
</div>

<style>
  .metrics {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--s3);
    margin-bottom: var(--s4);
  }
  .metric {
    position: relative;
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s3) var(--s4);
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow: hidden;
    transition: border-color 0.12s ease;
    color: inherit;
  }
  a.metric:hover {
    border-color: var(--line-strong);
    color: inherit;
  }
  .metric::before {
    content: '';
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--line-strong);
  }
  .metric.live::before {
    background: var(--st-running);
  }
  .metric.alert::before {
    background: var(--bad);
  }
  .k {
    font-size: 0.74rem;
    color: var(--fg-faint);
  }
  .metric strong {
    font-size: 1.7rem;
    font-weight: 600;
    letter-spacing: -0.02em;
    line-height: 1.2;
  }
  .metric.live strong {
    color: var(--st-running);
  }
  .metric.alert strong {
    color: var(--bad);
  }
  .metric .hint {
    font-size: 0.72rem;
    color: var(--fg-faint);
  }

  .urgent {
    border-color: color-mix(in srgb, var(--warn) 45%, var(--line));
    background: color-mix(in srgb, var(--warn) 4%, var(--surface-1));
    margin-bottom: var(--s4);
  }

  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--s4);
  }
  .cols .wide {
    grid-column: 1 / -1;
  }
  @media (max-width: 1000px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }

  .list {
    list-style: none;
    margin: 0 calc(-1 * var(--s2));
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .list a {
    display: flex;
    align-items: center;
    gap: var(--s3);
    padding: 0.5rem var(--s2);
    border-radius: var(--r2);
    font-size: 0.85rem;
    transition: background 0.1s ease;
    min-width: 0;
    color: inherit;
  }
  .list a:hover {
    background: var(--surface-2);
  }
  .main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.35;
  }
  .err {
    font-size: 0.74rem;
    color: var(--bad);
    max-width: 40ch;
  }
  .clock {
    font-size: 0.8rem;
    color: var(--warn);
  }
  .clock.soon {
    color: var(--bad);
  }
  .cron {
    color: var(--fg-dim);
  }
  .next {
    font-size: 0.8rem;
    color: var(--accent-fg);
  }
</style>
