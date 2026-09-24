<script lang="ts">
  /**
   * 概览。
   *
   * 回答四个问题，按紧急程度排：**有什么卡住等人？现在有什么在跑？最近有什么坏了？
   * 接下来什么时候会自己跑？**一个执行控制平面的首页不该是"任务列表"——
   * 那是配置视角，不是运行视角。
   *
   * 数字全部来自服务端聚合（`/overview`、`/overview/daily`、`/overview/tasks`）。
   * 之前是拉最近 200 条 run 回这里自己算，于是"24 小时"那几个数在实例忙起来之后
   * 就是错的——第 201 条之后的静静地不算，而界面上看不出来。窗口聚合只能在数据库里做。
   *
   * 成功率、趋势、排行都**不含影子执行**：影子执行是在试提示词，它失败了不代表任务坏了。
   */
  import { session } from '$lib/auth/session.svelte';
  import { describeError } from '$api/client';
  import { invalidate, resource } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import { listRuns } from '$api/runs';
  import { getFailingTasks, getOverview, getOverviewDaily } from '$api/overview';
  import type { Overview } from '$api/types/Overview';
  import type { OverviewDaily } from '$api/types/OverviewDaily';
  import type { OverviewTasks } from '$api/types/OverviewTasks';
  import type { RunListItem } from '$api/types/RunListItem';
  import type { RunStatus } from '$api/types/RunStatus';
  import { rerun as rerunRun } from '$lib/runs/rerun';
  import { successRate } from '$lib/charts/daily';
  import { describeCron } from '$lib/schedules/cron';
  import DailyChart from '$lib/overview/DailyChart.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import Sparkline from '$lib/ui/Sparkline.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import {
    DISPLAY_TIMEZONE,
    FAILED_STATUSES,
    ago,
    duration,
    mmss,
    money,
    moneyMicros,
    stamp,
    toMicros,
    triggerLabel,
    until
  } from '$lib/ui/format';

  const DAYS = 14;
  /** "失败最多的任务"看一周：一天的样本太少，一个月又会把早就修好的翻出来。 */
  const RANK_HOURS = 24 * 7;

  // 3 秒是人盯着屏幕时不会觉得卡顿的节奏。标签页隐藏时共享的心跳会自己停。
  const overview = resource<Overview>('overview', (signal) => getOverview(signal), { pollMs: 3000 });
  // 下面三份变化慢，一分钟拉一次；run 结束时 SSE 会按 `overview` 前缀让它们一起失效。
  const dailyRes = resource<OverviewDaily>(
    'overview:daily',
    (signal) => getOverviewDaily(DAYS, DISPLAY_TIMEZONE, signal),
    { pollMs: 60_000 }
  );
  const failingRes = resource<OverviewTasks>(
    'overview:tasks',
    (signal) => getFailingTasks(RANK_HOURS, 5, signal),
    { pollMs: 60_000 }
  );
  const failedRes = resource<RunListItem[]>(
    'overview:failed',
    async (signal) => (await listRuns({ status: FAILED_STATUSES as RunStatus[], limit: 6 }, signal)).items,
    { pollMs: 10_000 }
  );
  // 和 /approvals、侧栏徽标共用一个 key，三处订阅只发一条请求
  const approvalsRes = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });

  const stats = $derived(overview.data?.stats);
  const live = $derived(overview.data?.live ?? []);
  const upcoming = $derived(overview.data?.upcoming ?? []);
  const approvals = $derived(approvalsRes.data ?? []);
  const days = $derived(dailyRes.data?.days ?? []);
  const failing = $derived(failingRes.data?.items ?? []);
  const failed = $derived(failedRes.data ?? []);
  const loaded = $derived(!overview.pending);
  const error = $derived(overview.error ? describeError(overview.error) : null);
  const canOperate = $derived(session.can('operator'));
  const windowHours = $derived(stats?.window_hours ?? 24);

  // 停在审批门上的 run 状态还是 running，单独标出来：那是在等人，不是在跑
  const awaitingRuns = $derived(new Set(approvals.map((a) => a.run_id)));

  // 进行中的耗时、定时的倒数都跟着这个走
  let now = $state(Date.now());
  $effect(() => {
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  const rate = $derived.by(() => {
    const o = stats?.outcomes;
    if (!o || o.succeeded + o.failed === 0) return null;
    return o.succeeded / (o.succeeded + o.failed);
  });
  const percent = (r: number) => `${Math.round(r * 100)}%`;
  const rateSeries = $derived(days.map((d) => ({ date: d.date, value: successRate(d) })));
  const spendSeries = $derived(days.map((d) => ({ date: d.date, value: toMicros(d.spend_usd) })));
  const spendTotal = $derived(spendSeries.reduce((sum, p) => sum + (p.value ?? 0), 0));
  const soonestExpiry = $derived(approvals.length ? Math.min(...approvals.map((a) => a.expires_in_s)) : null);

  function elapsed(r: { status: string; created_at: string; started_at?: string | null }) {
    void now;
    return r.status === 'queued' ? `排队 ${duration(r.created_at)}` : duration(r.started_at ?? r.created_at);
  }

  /**
   * 重跑留在首页：新 run 马上出现在"正在执行"里，点进去就能看。
   * 跳走的话，一口气重跑几条失败的就得来回点好几次。
   */
  let busy = $state<string | null>(null);
  async function rerun(run: RunListItem) {
    busy = run.id;
    try {
      await rerunRun(run);
      toast(`已重新触发「${run.task_name}」`);
      invalidate('overview');
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }
</script>

<PageHeader title="概览">
  {#snippet actions()}
    {#if canOperate}<a class="btn btn-primary" href="/tasks/new">新建任务</a>{/if}
  {/snippet}
</PageHeader>

{#if error}<div class="banner">{error}</div>{/if}

<section class="tiles">
  <a class="tile" href="/runs?status=live">
    <span class="k">正在执行</span>
    <strong class:live={(stats?.running ?? 0) > 0}>{(stats?.running ?? 0) + (stats?.queued ?? 0)}</strong>
    <span class="hint">
      {#if stats?.running || stats?.queued}
        {stats.running} 个在跑{stats.queued ? ` · ${stats.queued} 个排队` : ''}
      {:else}
        空闲
      {/if}
    </span>
  </a>
  <a class="tile" href="/approvals">
    <span class="k">等待审批</span>
    <strong class:warn={(stats?.pending_approvals ?? 0) > 0}>{stats?.pending_approvals ?? 0}</strong>
    <span class="hint">
      {#if soonestExpiry !== null}
        最早一个 <span class="num-inline">{mmss(soonestExpiry)}</span> 后超时，按拒绝处理
      {:else}
        没有卡住的
      {/if}
    </span>
  </a>
  <a class="tile" href="/runs?status=failed&range=24h">
    <span class="k">{windowHours}h 成功率</span>
    <strong>{rate === null ? '—' : percent(rate)}</strong>
    <span class="hint">
      {#if stats && rate !== null}
        <span class:bad-text={stats.outcomes.failed > 0}>失败 {stats.outcomes.failed}</span>
        · 共 {stats.outcomes.succeeded + stats.outcomes.failed} 次结束
      {:else}
        {windowHours} 小时内没有结束的执行
      {/if}
    </span>
    {#if days.length}
      <div class="spark">
        <Sparkline points={rateSeries} format={percent} domain={[0, 1]} label="近 {DAYS} 天每天的成功率" />
      </div>
    {/if}
  </a>
  <div class="tile">
    <span class="k">{windowHours}h 花费</span>
    <strong>{money(stats?.spend_usd ?? '0')}</strong>
    <span class="hint">近 {DAYS} 天 {moneyMicros(spendTotal)}，含影子执行</span>
    {#if days.length}
      <div class="spark">
        <Sparkline points={spendSeries} format={moneyMicros} label="近 {DAYS} 天每天的花费" />
      </div>
    {/if}
  </div>
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
          <a class="item" href="/runs/{a.run_id}">
            <StatusBadge status="awaiting_approval" variant="dot" />
            <span class="main ellipsis">{a.title}</span>
            <span class="spacer"></span>
            <span class="faint ellipsis side">{live.find((r) => r.id === a.run_id)?.task_name ?? ''}</span>
            <span class="clock" class:soon={a.expires_in_s < 60}>剩余 {mmss(a.expires_in_s)}</span>
          </a>
        </li>
      {/each}
    </ul>
  </section>
{/if}

<section class="card trend">
  <header class="card-head">
    <h2>近 {DAYS} 天</h2>
    <span class="sub">按东八区的自然日；次数不含影子执行</span>
  </header>
  {#if dailyRes.pending}
    <Loading rows={4} />
  {:else if dailyRes.error}
    <p class="faint">{describeError(dailyRes.error)}</p>
  {:else}
    <DailyChart {days} />
  {/if}
</section>

<div class="cols">
  <section class="card">
    <header class="card-head">
      <h2>正在执行</h2>
      <span class="spacer"></span>
      <a class="btn btn-ghost btn-sm" href="/runs?status=live">全部</a>
    </header>
    {#if !loaded}
      <Loading rows={3} />
    {:else if live.length}
      <ul class="list">
        {#each live as r (r.id)}
          <li>
            <a class="item" href="/runs/{r.id}">
              <StatusBadge status={r.status === 'running' && awaitingRuns.has(r.id) ? 'awaiting_approval' : r.status} variant="dot" />
              <span class="main ellipsis">{r.task_name}</span>
              {#if r.dry_run}<span class="tag accent">影子</span>{/if}
              <span class="spacer"></span>
              <span class="faint nowrap">{triggerLabel(r.trigger)}</span>
              <span class="elapsed" title={stamp(r.started_at ?? r.created_at)}>{elapsed(r)}</span>
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
      <h2>最近失败</h2>
      <span class="spacer"></span>
      <a class="btn btn-ghost btn-sm" href="/runs?status=failed">全部</a>
    </header>
    {#if failedRes.pending}
      <Loading rows={3} />
    {:else if failed.length}
      <ul class="list">
        {#each failed as r (r.id)}
          <li class="with-act">
            <a class="item" href="/runs/{r.id}">
              <StatusBadge status={r.status} variant="dot" />
              <span class="main">
                <span class="ellipsis">
                  {r.task_name}
                  {#if r.dry_run}<span class="tag accent">影子</span>{/if}
                </span>
                {#if r.error}<span class="err ellipsis" title={r.error}>{r.error}</span>{/if}
              </span>
              <span class="spacer"></span>
              <span class="faint nowrap" title={stamp(r.finished_at ?? r.created_at)}>{ago(r.finished_at ?? r.created_at)}</span>
            </a>
            {#if canOperate}
              <button
                class="btn-ghost btn-sm btn-icon"
                title="用同样的输入再跑一次"
                aria-label="重跑「{r.task_name}」"
                disabled={busy === r.id}
                onclick={() => rerun(r)}
              >
                <Icon name="refresh" />
              </button>
            {/if}
          </li>
        {/each}
      </ul>
    {:else}
      <Empty compact title="最近没有失败" hint="失败、超时、超预算、超资源限制的执行会列在这里。" />
    {/if}
  </section>

  <section class="card">
    <header class="card-head">
      <h2>失败最多的任务</h2>
      <span class="sub">近 7 天</span>
    </header>
    {#if failingRes.pending}
      <Loading rows={3} />
    {:else if failing.length}
      <ul class="list">
        {#each failing as t (t.task_id)}
          {@const recovered = t.last_status === 'succeeded'}
          <li>
            <a class="item" href="/tasks/{t.task_id}">
              <span class="main">
                <span class="ellipsis">{t.task_name}</span>
                <!-- 已经恢复的，上一次失败的原因只是个参考，不再标红 -->
                {#if t.last_error}<span class="err ellipsis" class:settled={recovered} title={t.last_error}>{t.last_error}</span>{/if}
              </span>
              <span class="spacer"></span>
              <span class="count">
                <b class="bad-text">{t.failed}</b> <span class="faint">/ {t.runs} 次</span>
              </span>
              <span class="last nowrap" title="最近一次：{stamp(t.last_run_at)}">
                {#if recovered}
                  <StatusBadge status="succeeded" label="已恢复" variant="text" />
                {:else}
                  <span class="faint">{ago(t.last_failed_at)}失败</span>
                {/if}
              </span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty compact title="近 7 天没有任务失败" />
    {/if}
  </section>

  <section class="card">
    <header class="card-head">
      <h2>接下来会触发</h2>
    </header>
    {#if !loaded}
      <Loading rows={2} />
    {:else if upcoming.length}
      <ul class="list">
        {#each upcoming as s (s.schedule_id)}
          <li>
            <a class="item" href="/tasks/{s.task_id}">
              <span class="main">
                <span class="ellipsis">{s.task_name}</span>
                <span class="faint ellipsis small" title={s.cron}>{describeCron(s.cron)}</span>
              </span>
              <span class="spacer"></span>
              <span class="next nowrap" title={stamp(s.next_fire_at)}>{until(s.next_fire_at, now)}</span>
            </a>
          </li>
        {/each}
      </ul>
    {:else}
      <Empty compact title="没有启用中的定时" hint="在任务详情页给任务配上定时，它就会出现在这里。" />
    {/if}
  </section>
</div>

<style>
  .tiles {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--s3);
    margin-bottom: var(--s4);
  }
  @media (max-width: 960px) {
    .tiles {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
  .tile {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    box-shadow: var(--shadow-card);
    padding: var(--s3) var(--s4);
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    min-width: 0;
    color: inherit;
    transition:
      border-color var(--dur-2) var(--ease),
      background var(--dur-2) var(--ease);
  }
  a.tile:hover {
    border-color: var(--line-strong);
    background: var(--surface-hover);
    color: inherit;
  }
  .k {
    font-size: var(--t-sm);
    font-weight: 500;
    color: var(--fg-dim);
  }
  .tile strong {
    font-size: var(--t-3xl);
    font-weight: 600;
    letter-spacing: -0.02em;
    line-height: 1.15;
    font-variant-numeric: tabular-nums;
  }
  .tile strong.live {
    color: var(--info-fg);
  }
  .tile strong.warn {
    color: var(--warn-fg);
  }
  .tile .hint {
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .num-inline {
    font-variant-numeric: tabular-nums;
  }
  .bad-text {
    color: var(--bad-fg);
  }
  /* 迷你图贴着卡片底部：有图的卡片和没图的并排时，数字仍然在同一条线上 */
  .spark {
    margin-top: auto;
    padding-top: var(--s2);
  }

  .urgent {
    border-color: var(--warn-border);
    margin-bottom: var(--s4);
  }
  .urgent .card-head h2::before {
    content: '';
    display: inline-block;
    width: 7px;
    height: 7px;
    margin-right: var(--s2);
    border-radius: 50%;
    background: var(--warn-fg);
    vertical-align: middle;
    animation: pulse 1.6s ease-in-out infinite;
  }
  .trend {
    margin-bottom: var(--s4);
  }

  /* 不拉齐高度：一边空着的时候，被撑高的空卡片比参差的底边难看 */
  .cols {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    align-items: start;
    gap: var(--s4);
  }
  @media (max-width: 960px) {
    .cols {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .list {
    list-style: none;
    margin: 0 calc(-1 * var(--s2));
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .list li {
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .list li + li {
    border-top: 1px solid var(--line);
  }
  .item {
    flex: 1;
    display: flex;
    align-items: center;
    gap: var(--s3);
    padding: 0.55rem var(--s2);
    border-radius: var(--r2);
    font-size: var(--t-base);
    transition: background var(--dur-1) var(--ease);
    min-width: 0;
    color: inherit;
  }
  .item:hover {
    background: var(--surface-hover);
    color: inherit;
  }
  .with-act button {
    flex: 0 0 auto;
    margin-right: var(--s1);
  }
  .main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.35;
    font-weight: 500;
  }
  .main .small {
    font-size: var(--t-sm);
    font-weight: 400;
  }
  .main .tag {
    margin-left: var(--s1);
  }
  .side {
    max-width: 14ch;
  }
  .clock {
    font-size: var(--t-sm);
    color: var(--warn-fg);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .clock.soon {
    color: var(--bad-fg);
  }
  .elapsed {
    font-size: var(--t-sm);
    color: var(--info-fg);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .err.settled {
    color: var(--fg-faint);
  }
  .count {
    font-size: var(--t-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .last {
    font-size: var(--t-sm);
  }
  .next {
    font-size: var(--t-sm);
    color: var(--accent-fg);
  }
</style>
