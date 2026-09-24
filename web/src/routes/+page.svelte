<script lang="ts">
  /**
   * 概览。
   *
   * 回答四个问题，按紧急程度排：**有什么卡住等人？现在有什么在跑？最近有什么坏了？
   * 接下来什么时候会自己跑？**一个执行控制平面的首页不该是"任务列表"——
   * 那是配置视角，不是运行视角。
   *
   * 数字全部来自 `GET /api/v1/overview`。之前是拉最近 200 条 run 回这里自己算，
   * 于是"24 小时"那三个数在实例忙起来之后就是错的——第 201 条之后的静静地不算，
   * 而界面上看不出来。窗口聚合只能在数据库里做。
   */
  import { session } from '$lib/auth/session.svelte';
  import { api, describeError } from '$api/client';
  import { resource } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import type { Overview } from '$api/types/Overview';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { ago, money, mmss, stamp, triggerLabel } from '$lib/ui/format';

  // 3 秒是人盯着屏幕时不会觉得卡顿的节奏。标签页隐藏时共享的心跳会自己停。
  const overview = resource<Overview>(
    'overview',
    (signal) => api<Overview>('/api/v1/overview', { signal }),
    { pollMs: 3000 }
  );
  // 审批卡片要倒计时，和首页其它数据分开拉：它在 /approvals 和 Shell 里也用同一个 key，
  // 三处订阅只会产生一条请求。
  const approvalsRes = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });

  const stats = $derived(overview.data?.stats);
  const live = $derived(overview.data?.live ?? []);
  const recent = $derived(overview.data?.recent ?? []);
  const upcoming = $derived(overview.data?.upcoming ?? []);
  const approvals = $derived(approvalsRes.data ?? []);
  const loaded = $derived(!overview.pending);
  const error = $derived(overview.error ? describeError(overview.error) : null);

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
    {#if session.can('operator')}<a class="btn btn-primary" href="/tasks/new">新建任务</a>{/if}
  {/snippet}
</PageHeader>

{#if error}<div class="banner">{error}</div>{/if}

<section class="metrics">
  <a class="metric" class:live={(stats?.running ?? 0) > 0} href="/runs?status=live">
    <span class="k">正在执行</span>
    <strong>{stats?.running ?? 0}</strong>
    <span class="hint">
      {stats?.queued ? `另有 ${stats.queued} 个排队` : stats?.running ? '点开看进度' : '空闲'}
    </span>
  </a>
  <a class="metric" class:alert={(stats?.pending_approvals ?? 0) > 0} href="/approvals">
    <span class="k">等待审批</span>
    <strong>{stats?.pending_approvals ?? 0}</strong>
    <span class="hint">{stats?.pending_approvals ? '超时会按拒绝处理' : '没有卡住的'}</span>
  </a>
  <a class="metric" class:alert={(stats?.failed ?? 0) > 0} href="/runs?status=failed">
    <span class="k">{stats?.window_hours ?? 24}h 失败</span>
    <strong>{stats?.failed ?? 0}</strong>
    <span class="hint">共 {stats?.runs ?? 0} 次执行</span>
  </a>
  <div class="metric">
    <span class="k">{stats?.window_hours ?? 24}h 花费</span>
    <strong>{money(stats?.spend_usd ?? '0')}</strong>
    <span class="hint">按 run 累计的模型费用</span>
  </div>
  <a class="metric" href="/tasks">
    <span class="k">任务</span>
    <strong>{stats?.tasks ?? 0}</strong>
    <span class="hint">去任务页配置和触发</span>
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
            <span class="faint">{live.find((r) => r.id === a.run_id)?.task_name ?? ''}</span>
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
              <span class="main ellipsis">{r.task_name}</span>
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
                <span class="ellipsis">{r.task_name}</span>
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
          {#if session.can('operator')}<a class="btn" href="/tasks/new">建一个任务</a>{/if}
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
        {#each upcoming as s (s.schedule_id)}
          <li>
            <a href="/tasks/{s.task_id}">
              <span class="dot ready"></span>
              <span class="main ellipsis">{s.task_name}</span>
              <code class="cron">{s.cron}</code>
              <span class="spacer"></span>
              <span class="faint" title={stamp(s.next_fire_at)}>{stamp(s.next_fire_at)}</span>
              <span class="next">{until(s.next_fire_at)}</span>
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
    transition: border-color var(--dur-2) var(--ease);
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
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .metric strong {
    font-size: var(--t-3xl);
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
    font-size: var(--t-xs);
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
  @media (max-width: 960px) {
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
    font-size: var(--t-base);
    transition: background var(--dur-1) var(--ease);
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
  .clock {
    font-size: var(--t-sm);
    color: var(--warn);
  }
  .clock.soon {
    color: var(--bad);
  }
  .cron {
    color: var(--fg-dim);
  }
  .next {
    font-size: var(--t-sm);
    color: var(--accent-fg);
  }
</style>
