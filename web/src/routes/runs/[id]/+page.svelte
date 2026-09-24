<script lang="ts">
  /**
   * 一次执行。
   *
   * 进来第一眼要回答：**结论是什么、错在哪一步、还要不要我做什么。**
   * 所以页头讲清是哪个任务、什么状态、花了多少、跑了多久；紧接着是等人点头的审批、
   * 失败原因或结果；再往下左边是步骤导航，右边是过程、编排图、资源、漂移、原始事件。
   *
   * 事件是按帧批量提交的：一次长执行有上万条事件，每来一条就重算一遍整页，
   * 跑得越久越卡。现在事件先进队列，每一帧最多处理约 8ms，剩下的留到下一帧。
   */
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { tick, untrack } from 'svelte';
  import { describeError, ignoreForbidden } from '$api/client';
  import { session } from '$lib/auth/session.svelte';
  import { resource, invalidate } from '$api/resource.svelte';
  import { subscribeRunEvents, type StreamStatus } from '$api/events';
  import {
    cancelRun,
    deleteRun,
    getRun,
    getTaskVersion,
    triggerRun,
    newIdempotencyKey
  } from '$api/runs';
  import { listApprovals, listHosts, type Approval } from '$api/models';
  import type { RunDetail } from '$api/types/RunDetail';
  import type { RunEvent } from '$api/types/RunEvent';
  import type { TaskVersion } from '$api/types/TaskVersion';
  import { ProcessBuilder, RunTally, isOpen, nodeNames, type NavStep, type NodeRun } from '$lib/runs/process';
  import Process from '$lib/runs/Process.svelte';
  import StepNav from '$lib/runs/StepNav.svelte';
  import RunOutput from '$lib/runs/RunOutput.svelte';
  import RawEvents from '$lib/runs/RawEvents.svelte';
  import ResourceChart from '$lib/metrics/ResourceChart.svelte';
  import DriftPanel from '$lib/drift/DriftPanel.svelte';
  import DagView from '$lib/dag/DagView.svelte';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Tabs from '$lib/ui/Tabs.svelte';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import CopyButton from '$lib/ui/CopyButton.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Dropdown from '$lib/ui/Dropdown.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import {
    DISPLAY_TIMEZONE,
    FAILED_STATUSES,
    duration,
    isTerminal,
    moneyMicros,
    stamp,
    toMicros,
    triggerLabel
  } from '$lib/ui/format';
  import { scrollToNode } from '$lib/ui/scroll';

  const runId = $derived(page.params.id ?? '');

  let detail = $state<RunDetail | null>(null);
  let version = $state<TaskVersion | null>(null);
  let error = $state<string | null>(null);
  let streamStatus = $state<StreamStatus>('connecting');

  // ---------------------------------------------------------------- 事件
  //
  // 事件本身放在普通数组里，不做成深层 $state：那会给每条事件建代理，
  // 而且每来一条，所有依赖它的派生值都要重算一遍。界面只订阅下面几个"发布"出来的值。
  // `$state.raw`：只在整个换掉（切到另一个 run）时通知界面；平时原地 push，
  // 界面靠下面的 revision 知道该重读了。
  let events = $state.raw<RunEvent[]>([]);
  let builder = new ProcessBuilder();
  let tally = $state.raw(new RunTally());
  let queue: RunEvent[] = [];

  let nodes = $state.raw<NodeRun[]>([]);
  let revision = $state(0);
  let live = $state.raw(snapshotTally());

  function snapshotTally() {
    return {
      status: tally.status,
      startedAt: tally.startedAt,
      finishedAt: tally.finishedAt,
      costMicros: tally.costMicros,
      finishedNodes: tally.finishedNodes,
      degraded: tally.degraded,
      total: tally.total
    };
  }

  let frame: number | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  /**
   * 打开页面时已经有的最大 seq。在它之前的是历史回放，不该把人从页顶拽到底部——
   * 等审批的 run 最要紧的就是页顶那张审批卡。之后新来的事件才跟着滚。
   */
  let followAfter: number | null = null;

  /** 攒到下一帧再处理；标签页在后台时浏览器不跑 rAF，改用定时器。 */
  function schedule() {
    if (frame !== null || timer !== null) return;
    if (document.visibilityState === 'hidden') timer = setTimeout(flush, 250);
    else frame = requestAnimationFrame(flush);
  }

  function cancelFlush() {
    if (frame !== null) cancelAnimationFrame(frame);
    if (timer !== null) clearTimeout(timer);
    frame = null;
    timer = null;
  }

  /**
   * 处理队列。每一帧最多占约 8ms：打开一个已经跑完的长 run 时，历史事件会一口气
   * 涌进来，一次处理完会把主线程卡住几百毫秒——页面看起来像死了。
   */
  function flush() {
    frame = null;
    timer = null;
    const started = performance.now();
    let processed = 0;
    while (processed < queue.length) {
      const event = queue[processed++];
      events.push(event);
      builder.push(event);
      tally.push(event);
      if ((processed & 127) === 0 && performance.now() - started > 8) break;
    }
    queue = processed >= queue.length ? [] : queue.slice(processed);
    publish();
    if (queue.length) schedule();
  }

  function publish() {
    if (builder.changed) nodes = builder.snapshot();
    live = snapshotTally();
    revision += 1;
    const latest = events.length ? events[events.length - 1].seq : 0;
    if (follow && tab === 'process' && !terminal && followAfter !== null && latest > followAfter)
      void tick().then(() => tail?.scrollIntoView({ block: 'end' }));
  }

  function reset() {
    cancelFlush();
    followAfter = null;
    events = [];
    builder = new ProcessBuilder(version ? nodeNames(version.spec) : {});
    tally = new RunTally();
    queue = [];
    nodes = [];
    live = snapshotTally();
    revision += 1;
  }

  $effect(() => {
    const id = runId;
    if (!id) return;
    // 会话过期时 EventSource 被 401 永久关掉；重新登录后 epoch 变了，这里重新订阅一遍
    void session.epoch;
    // 从一个 run 直接跳到另一个 run 时组件是复用的：一切从零开始
    untrack(() => {
      detail = null;
      version = null;
      error = null;
      reset();
    });

    getRun(id)
      .then((d) => {
        if (id !== runId) return;
        detail = d;
        followAfter = d.max_seq;
      })
      .catch((e) => (error = describeError(e)));

    const stream = subscribeRunEvents(id, {
      onEvent(event) {
        queue.push(event);
        schedule();
        if (event.body.kind === 'run_finished') {
          // 终态刷新拿最终成本、耗时和输出。失败不致命（流里的数据还在），但得说出来
          getRun(id)
            .then((d) => {
              if (id === runId) detail = d;
            })
            .catch((e) => toastError(describeError(e)));
          // **SSE 是失效信号，缓存是存储。**这个 run 结束了，首页和执行列表立刻就旧了
          invalidate('overview', 'runs');
        }
        if (event.body.kind === 'approval_requested' || event.body.kind === 'approval_decided') {
          invalidate('approvals', 'overview');
        }
      },
      onStatus: (s) => (streamStatus = s)
    });

    return () => {
      stream.close();
      cancelFlush();
    };
  });

  // 编排图和步骤名要用**这次执行用的那一版**。任务改过之后，拿当前定义去画老 run，
  // 图和步骤名都是错的。版本不可变，取一次就缓存着。
  $effect(() => {
    const d = detail;
    if (!d) return;
    getTaskVersion(d.task_id, d.version_no)
      .then((v) => {
        if (d.id !== runId) return;
        version = v;
        builder.setNames(nodeNames(v.spec));
        publish();
      })
      .catch((e) => toastError(describeError(e)));
  });

  // 主机是 admin 才能读；读不到就在过程里显示短 id，不该报错
  let hosts = $state<Array<{ id: string; name: string }>>([]);
  $effect(() => {
    listHosts()
      .then((h) => (hosts = h))
      .catch(ignoreForbidden);
  });

  // 本 run 的待审批。审批门挂着时 run 停在 running，人得能就地点头。
  // 和侧栏徽标、首页、/approvals 共享同一个 key：几处订阅只产生一条请求
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });
  const pending = $derived((approvals.data ?? []).filter((a) => a.run_id === runId));
  /** 在等人的审批节点。审批门等人时只有 node_started，结论要等审批结束才写，只能从待审批列表里知道。 */
  const waiting = $derived(new Set(pending.map((a) => a.node_key).filter((k): k is string => !!k)));
  /** 待审批列表 5 秒一刷，可能比事件晚：事件里已经结束的节点不再算在等。 */
  const awaiting = (key: string, status: string | null | undefined) =>
    waiting.has(key) && isOpen(status);

  // ---------------------------------------------------------------- 派生

  /** 事件比接口更新：执行中以事件推出来的状态为准。 */
  const status = $derived(live.status ?? detail?.status ?? null);
  const terminal = $derived(status !== null && isTerminal(status));
  const failedLike = $derived(
    status !== null && (FAILED_STATUSES.includes(status) || status === 'cancelled')
  );
  const costMicros = $derived(live.total > 0 ? live.costMicros : toMicros(detail?.cost_usd ?? '0'));
  const startedAt = $derived(detail?.started_at ?? live.startedAt);
  const finishedAt = $derived(detail?.finished_at ?? live.finishedAt);

  // 跑着的时候耗时要走
  let now = $state(Date.now());
  $effect(() => {
    if (terminal) return;
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const elapsed = $derived.by(() => {
    void now;
    return duration(startedAt, finishedAt);
  });

  const specNodes = $derived(version?.spec.nodes ?? []);
  const position = $derived(
    Object.fromEntries(specNodes.map((n, i) => [String(n.key), i + 1])) as Record<string, number>
  );

  /** 编排里有、但事件里还没出现的步骤。 */
  const notStarted = $derived.by(() => {
    const seen = new Set(nodes.map((n) => n.key));
    return specNodes
      .filter((n) => !seen.has(String(n.key)))
      .map((n) => ({
        key: String(n.key),
        name: n.name || String(n.key),
        waiting: waiting.has(String(n.key))
      }));
  });

  const navSteps = $derived.by((): NavStep[] => {
    const byKey = new Map(nodes.map((n) => [n.key, n]));
    const out: NavStep[] = [];
    const seen = new Set<string>();
    for (const spec of specNodes) {
      const key = String(spec.key);
      seen.add(key);
      const n = byKey.get(key);
      out.push({
        key,
        name: spec.name || key,
        status: awaiting(key, n?.status) ? 'awaiting_approval' : (n?.status ?? 'pending'),
        startedAt: n?.startedAt ?? null,
        finishedAt: n?.finishedAt ?? null,
        costMicros: n?.costMicros ?? 0
      });
    }
    // 编排里没有、事件里有的（map 实例之类），按出现顺序补在后面
    for (const n of nodes) {
      if (seen.has(n.key)) continue;
      out.push({
        key: n.key,
        name: n.name,
        status: n.status ?? 'pending',
        startedAt: n.startedAt,
        finishedAt: n.finishedAt,
        costMicros: n.costMicros
      });
    }
    return out;
  });

  /** 编排图上每个节点的状态：事件里的状态，再叠上正在等人的审批节点。 */
  const dagStatus = $derived.by(() => {
    const out: Record<string, string> = {};
    for (const n of nodes) if (n.status) out[n.key] = n.status;
    for (const key of waiting) if (awaiting(key, out[key])) out[key] = 'awaiting_approval';
    return out;
  });

  /** 出错的那一步：失败原因旁边给个链接直接跳过去。 */
  const failing = $derived(
    nodes.find((n) => n.status === 'failed') ?? nodes.find((n) => n.error !== null)
  );
  /** 没有哪一步出错、run 却没跑完（重启回收之类）：指到停下的那一步。 */
  const stoppedAt = $derived(failing ? undefined : nodes.find((n) => n.status === 'interrupted'));

  // ---------------------------------------------------------------- 页签与定位

  type Tab = 'process' | 'graph' | 'resources' | 'drift' | 'events';
  let tab = $state<Tab>('process');
  const TABS = $derived([
    { id: 'process' as Tab, label: '执行过程' },
    { id: 'graph' as Tab, label: '编排图' },
    { id: 'resources' as Tab, label: '资源' },
    { id: 'drift' as Tab, label: '漂移' },
    { id: 'events' as Tab, label: '原始事件', count: live.total }
  ]);

  /** 跟着最新的输出滚。往上翻就自动停下，免得读着读着被拽到底下去。 */
  let follow = $state(true);
  let tail = $state<HTMLElement | null>(null);
  function onWheel(event: WheelEvent) {
    if (event.deltaY < 0 && follow && !terminal) follow = false;
  }

  async function jumpTo(key: string) {
    follow = false;
    tab = 'process';
    await tick();
    scrollToNode(key);
  }

  // ---------------------------------------------------------------- 动作

  let busy = $state(false);
  let confirmingCancel = $state(false);
  let confirmingDelete = $state(false);
  const canOperate = $derived(session.can('operator'));

  async function cancel() {
    busy = true;
    try {
      await cancelRun(runId);
      toast('已发出取消，正在收尾');
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = false;
    }
  }

  async function removeRun() {
    busy = true;
    try {
      await deleteRun(runId);
      toast('已删除这条执行记录');
      await goto('/runs');
    } catch (e) {
      toastError(describeError(e));
      busy = false;
    }
  }

  /**
   * 再跑一次。**原样带上这次的输入**——以前只带了 dry_run，有输入的任务重跑直接失败。
   * 影子重跑拿这一次当基线（compare_to），跑完就能看到逐字段的差异。
   */
  async function rerun(shadow: boolean) {
    if (!detail) return;
    busy = true;
    try {
      // "再跑一次"每次点都是一次新的意图，每次拿一个新键
      const r = await triggerRun(
        detail.task_id,
        {
          dry_run: shadow || detail.dry_run,
          inputs: detail.inputs ?? undefined,
          compare_to: shadow ? runId : undefined
        },
        newIdempotencyKey()
      );
      toast(shadow ? '已开始影子重跑，跑完会和这次比较' : '已重新触发');
      await goto(`/runs/${r.id}`);
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = false;
    }
  }

  const STREAM_LABEL: Record<StreamStatus, string> = {
    connecting: '连接中…',
    open: '实时',
    retrying: '重连中…',
    closed: '已断开'
  };
</script>

<svelte:window onwheel={onWheel} />

<PageHeader
  title={detail?.task_name ?? `执行 ${runId.slice(0, 8)}`}
  crumbs={[{ label: '执行记录', href: '/runs' }]}
>
  {#if status}<StatusBadge {status} size="lg" />{/if}
  {#if detail?.dry_run}<span class="tag accent">影子执行</span>{/if}
  {#if status && !terminal}
    <!-- 连接状态要一直可见：断开时看到的是一份不再更新的快照，不标出来人会以为"卡住了" -->
    <span class="stream {streamStatus}">{STREAM_LABEL[streamStatus]}</span>
  {/if}
  {#snippet sub()}
    {#if detail}
      <span>
        <a class="link" href="/tasks/{detail.task_id}">v{detail.version_no}</a>
        {#if detail.current_version_no !== detail.version_no}
          <span class="faint">（任务现在是 v{detail.current_version_no}）</span>
        {/if}
      </span>
      <span>{triggerLabel(detail.trigger)}触发{detail.triggered_by ? ` · ${detail.triggered_by}` : ''}</span>
      <span title="入队 {stamp(detail.created_at)}（{DISPLAY_TIMEZONE}）">开始 {startedAt ? stamp(startedAt) : '—'}</span>
      <span>耗时 {elapsed}</span>
      <span>花费 {moneyMicros(costMicros)}</span>
      <span><CopyButton text={runId} display={runId.slice(0, 8)} label="复制执行 id" /></span>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if detail}
      {#if !terminal}
        {#if canOperate}
          <button class="btn-danger" onclick={() => (confirmingCancel = true)} disabled={busy}>取消执行</button>
        {/if}
      {:else if canOperate}
        <button class:btn-primary={failedLike} onclick={() => rerun(false)} disabled={busy}>
          {detail.dry_run ? '再影子跑一次' : '重跑'}
        </button>
      {/if}
      <Dropdown label="更多" disabled={busy}>
        <a href="/tasks/{detail.task_id}" role="menuitem">
          查看任务
          <span class="hint">当前是 v{detail.current_version_no}</span>
        </a>
        {#if canOperate && terminal}
          <button onclick={() => rerun(true)}>
            影子重跑并对比
            <span class="hint">副作用只记录不落地，跑完和这一次逐字段比较</span>
          </button>
          <hr />
          <button class="danger" onclick={() => (confirmingDelete = true)}>
            删除这条记录
            <span class="hint">连同事件流和资源采样</span>
          </button>
        {/if}
      </Dropdown>
    {/if}
  {/snippet}
</PageHeader>

<Confirm
  open={confirmingCancel}
  onclose={() => (confirmingCancel = false)}
  title="取消这次执行？"
  danger
  confirmText="取消执行"
  {busy}
  onconfirm={cancel}
>
  <p>正在跑的步骤会被中断。已经落地的改动不会回滚，已经花掉的钱也不会退。</p>
</Confirm>

<Confirm
  open={confirmingDelete}
  onclose={() => (confirmingDelete = false)}
  title="删除这次执行记录？"
  danger
  confirmText="删除"
  {busy}
  onconfirm={removeRun}
>
  <p>连同它的 {live.total} 条事件和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。</p>
  <p class="warn-text">删掉之后拿不回来。</p>
</Confirm>

{#if error}<div class="banner">{error}</div>{/if}

{#if pending.length}
  <section class="gates">
    {#each pending as approval (approval.id)}
      <ApprovalCard
        {approval}
        taskName={detail?.task_name ?? null}
        ondecided={() => invalidate('approvals', 'overview')}
      />
    {/each}
  </section>
{/if}

{#if detail && terminal && failedLike}
  <section class="outcome bad">
    <div class="outcome-head">
      <StatusBadge status={status ?? 'failed'} />
      <strong>{status === 'cancelled' ? '已取消' : '没有跑完'}</strong>
      {#if failing}
        <button class="btn-ghost btn-sm" onclick={() => jumpTo(failing.key)}>
          出错的步骤：{failing.name} →
        </button>
      {:else if stoppedAt}
        <button class="btn-ghost btn-sm" onclick={() => jumpTo(stoppedAt.key)}>
          停在：{stoppedAt.name} →
        </button>
      {/if}
    </div>
    {#if detail.error}<p class="reason">{detail.error}</p>{/if}
  </section>
{:else if detail && status === 'succeeded' && detail.output !== null && detail.output !== undefined}
  <section class="outcome card">
    <header class="card-head">
      <h2>结果</h2>
      <span class="sub">最后一个成功步骤的输出</span>
    </header>
    <RunOutput output={detail.output} />
  </section>
{/if}

{#if !detail && !error}
  <div class="card"><Loading rows={5} /></div>
{:else if detail}
  <div class="layout">
    <aside class="nav">
      {#if navSteps.length}
        <StepNav steps={navSteps} onselect={jumpTo} />
      {/if}
    </aside>

    <div class="main">
      <Tabs tabs={TABS} bind:value={tab} />

      <div class="panel">
        {#if tab === 'process'}
          {#if !terminal}
            <label class="follow check">
              <input type="checkbox" bind:checked={follow} />
              <span>跟着最新的输出滚动</span>
            </label>
          {/if}
          {#if nodes.length || notStarted.length}
            <Process {nodes} pending={notStarted} {hosts} {position} {waiting} />
          {:else}
            <Empty
              compact
              title={streamStatus === 'open' ? '已连上，等第一条事件…' : streamStatus === 'closed' ? '没有收到任何事件' : '正在连接事件流…'}
            />
          {/if}
          <div bind:this={tail} class="tail"></div>
        {:else if tab === 'graph'}
          {#if version}
            <div class="card">
              <DagView spec={version.spec} status={dagStatus} onselect={jumpTo} />
            </div>
          {:else}
            <div class="card"><Loading rows={3} /></div>
          {/if}
        {:else if tab === 'resources'}
          <ResourceChart {runId} revision={live.finishedNodes} degraded={live.degraded} />
        {:else if tab === 'drift'}
          <DriftPanel {runId} revision={live.finishedNodes} />
        {:else}
          <RawEvents {events} {revision} kinds={tally.kinds} />
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .stream {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: var(--t-xs);
    font-weight: 500;
    color: var(--fg-faint);
  }
  .stream::before {
    content: '';
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--st-queued);
  }
  .stream.open {
    color: var(--info-fg);
  }
  .stream.open::before {
    background: var(--info-fg);
    animation: pulse 1.6s ease-in-out infinite;
  }
  .stream.closed {
    color: var(--warn-fg);
  }
  .stream.closed::before {
    background: var(--warn-fg);
  }

  .gates {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    margin-bottom: var(--s4);
  }

  .outcome {
    margin-bottom: var(--s4);
  }
  .outcome.bad {
    padding: var(--s3) var(--s4);
    border: 1px solid var(--bad-border);
    border-radius: var(--r3);
    background: var(--bad-bg);
  }
  .outcome-head {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .outcome-head .btn-ghost {
    margin-left: auto;
    color: var(--bad-fg);
  }
  .reason {
    margin: var(--s2) 0 0;
    font-size: var(--t-sm);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .layout {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr);
    gap: var(--s5);
    align-items: start;
  }
  .nav {
    position: sticky;
    top: var(--s4);
    max-height: calc(100vh - 2rem);
    overflow-y: auto;
  }
  .main {
    min-width: 0;
  }
  .panel {
    padding-top: var(--s4);
  }
  .follow {
    margin-bottom: var(--s3);
    color: var(--fg-dim);
  }
  .tail {
    height: 1px;
  }
  @media (max-width: 1100px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
    .nav {
      display: none;
    }
  }
</style>
