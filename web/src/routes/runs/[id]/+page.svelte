<script lang="ts">
  /**
   * 一次执行。
   *
   * 页头先把"是哪个任务、什么状态、花了多少、跑了多久"讲清楚；然后是等人点头的
   * 审批门（如果有）、给人读的执行过程、资源归因、漂移比较；原始事件流折在最后，
   * 排查时才展开。
   */
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { api, describeError } from '$api/client';
  import { subscribeRunEvents, type EventStream } from '$api/events';
  import { cancelRun, deleteRun, getRun, triggerRun } from '$api/runs';
  import { listApprovals, listHosts, type Approval } from '$api/models';
  import type { RunEvent } from '$api/types/RunEvent';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskDetail } from '$api/types/TaskDetail';
  import ResourceChart from '$lib/metrics/ResourceChart.svelte';
  import DriftPanel from '$lib/drift/DriftPanel.svelte';
  import Process from '$lib/runs/Process.svelte';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import StatusPill from '$lib/ui/StatusPill.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Dropdown from '$lib/ui/Dropdown.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { clock, DISPLAY_TIMEZONE, duration, isTerminal, money, stamp, triggerLabel } from '$lib/ui/format';

  const runId = $derived(page.params.id ?? '');

  let run = $state<RunSummary | null>(null);
  let events = $state<RunEvent[]>([]);
  let status = $state<'connecting' | 'open' | 'closed'>('connecting');
  let error = $state<string | null>(null);
  let stream: EventStream | null = null;

  // 终态到了就把 run 概要刷新一次，拿到最终成本和耗时
  $effect(() => {
    if (!runId) return;
    events = [];
    getRun(runId)
      .then((r) => (run = r))
      .catch((e) => (error = describeError(e)));

    stream = subscribeRunEvents(runId, {
      onEvent(event) {
        // 服务端保证 seq 连续，直接追加即可；断线重连也不会重复
        events.push(event);
        if (event.body.kind === 'run_finished') {
          getRun(runId)
            .then((r) => (run = r))
            .catch(() => {});
        }
      },
      onStatus: (s) => (status = s)
    });

    return () => stream?.close();
  });

  // 跑着的时候耗时要走：每秒重算一次页头的时长
  let tick = $state(0);
  $effect(() => {
    if (!run || isTerminal(run.status)) return;
    const timer = setInterval(() => (tick += 1), 1000);
    return () => clearInterval(timer);
  });
  const elapsed = $derived.by(() => {
    void tick;
    return duration(run?.started_at, run?.finished_at);
  });

  // 编排 + 实时状态叠在同一张图上：看一个正在跑的 run 时，
  // 不用在"图"和"事件流"之间来回对照。
  let task = $state<TaskDetail | null>(null);
  let hosts = $state<Array<{ id: string; name: string }>>([]);
  $effect(() => {
    if (!run?.task_id) return;
    api<TaskDetail>(`/api/v1/tasks/${run.task_id}`)
      .then((t) => (task = t))
      .catch(() => {});
  });
  $effect(() => {
    // 主机是 admin 才能读；读不到就在过程里显示短 id，不该报错
    listHosts()
      .then((h) => (hosts = h))
      .catch(() => {});
  });

  const cost = $derived(run?.cost_usd ?? '0.000000');

  // 降级的节点：它的曲线不完整，而且 limits 根本没生效
  const degraded = $derived.by(() => {
    const out: Record<string, { mode: string; detail: string | null }> = {};
    for (const event of events) {
      if (event.body.kind === 'resource_degraded' && event.node_key) {
        out[event.node_key] = { mode: event.body.mode, detail: event.body.detail ?? null };
      }
    }
    return out;
  });

  // 本 run 的待审批。审批门挂着时 run 停在 running，人得能就地点头，
  // 而不是先记住 run id 再翻到另一个页面去找。
  let pending = $state<Approval[]>([]);
  async function loadApprovals() {
    try {
      pending = (await listApprovals()).filter((a) => a.run_id === runId);
    } catch {
      /* 审批读不到不该把整个 run 页面弄坏 */
    }
  }
  $effect(() => {
    if (!runId) return;
    void loadApprovals();
    const timer = setInterval(loadApprovals, 3000);
    return () => clearInterval(timer);
  });

  // 节点跑完才有新的采样可读。用事件数当版本号，比定时轮询省事也更及时。
  const metricsRevision = $derived(
    events.filter((e) => e.body.kind === 'node_finished' || e.body.kind === 'run_finished').length
  );

  let busy = $state(false);
  let confirmingCancel = $state(false);
  let confirmingDelete = $state(false);

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

  /** 删这次执行记录。连事件流和资源采样一起，不可恢复。 */
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

  /** 同一个任务再跑一次。影子跑出来的就再影子跑一次，保持一致。 */
  async function again() {
    if (!run) return;
    busy = true;
    try {
      const r = await triggerRun(run.task_id, { dry_run: run.dry_run });
      toast('已重新触发');
      await goto(`/runs/${r.id}`);
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = false;
    }
  }

  async function copyId() {
    try {
      await navigator.clipboard.writeText(runId);
      toast('已复制 run id');
    } catch {
      toastError('复制失败');
    }
  }

  // 原始事件流是排查用的，**不过滤**：过滤掉的那类事件恰恰是出问题时想看的。
  // 这里的查找框只是"找"，不是"隐藏"——清空就全回来。
  let find = $state('');
  const shown = $derived.by(() => {
    const q = find.trim().toLowerCase();
    if (!q) return events;
    return events.filter(
      (e) =>
        e.body.kind.includes(q) ||
        (e.node_key ?? '').toLowerCase().includes(q) ||
        summarize(e).toLowerCase().includes(q)
    );
  });

  /** 未识别的事件类型降级成一行原始 JSON，不能崩也不能静默丢弃。 */
  function summarize(event: RunEvent): string {
    const b = event.body;
    switch (b.kind) {
      case 'run_queued': return `已入队（${triggerLabel(b.trigger)}${b.dry_run ? '，影子执行' : ''}）`;
      case 'run_started': return `worker ${b.worker} 开始执行`;
      case 'run_finished': return `结束：${b.status}${b.error ? ` — ${b.error}` : ''}`;
      case 'node_ready': return '依赖就绪';
      case 'node_started': return `开始（第 ${b.attempt} 次尝试）`;
      case 'node_finished': return `节点结束：${b.status}${b.error ? ` — ${b.error}` : ''}`;
      case 'node_retrying': return `第 ${b.attempt} 次失败，${b.delay_ms}ms 后重试：${b.reason}`;
      case 'node_skipped': return `跳过：${b.reason}`;
      case 'drift_detected': return `行为漂移：输入条件没变，${b.changed_paths.join('、')} 变了（基线 ${b.baseline_run_id.slice(0, 8)}）`;
      case 'resource_degraded': return `目标机只能按 ${b.mode} 记账，资源上限未强制${b.detail ? ` — ${b.detail}` : ''}`;
      case 'map_expanded': return `展开 ${b.count} 个实例（上游共 ${b.available} 项）`;
      case 'agent_text': return b.text;
      case 'agent_thinking': return b.text;
      // 提示词里的换行会把这一行撑成一屏。事件流是时间轴，一条事件一行；
      // 完整命令在上面的「执行命令」里
      case 'agent_invoked': {
        const flat = b.command.replace(/\s+/g, ' ');
        return `启动 AI：${flat.slice(0, 96)}${flat.length > 96 ? '…' : ''}`;
      }
      case 'agent_turn_started': return `第 ${b.turn} 轮`;
      case 'tool_requested': return `调用 ${b.tool} ${JSON.stringify(b.input).slice(0, 160)}`;
      case 'tool_completed': return `${b.ok ? '完成' : '失败'}：${b.output_preview}`;
      case 'policy_decided': return `策略 ${b.effect}：${b.reason}`;
      case 'usage': return `${b.model} · 入 ${b.input_tokens} / 出 ${b.output_tokens} · $${b.cost_usd}`;
      case 'approval_requested': return `等待审批：${b.title}`;
      case 'approval_decided': return `审批${b.approved ? '通过' : '拒绝'}${b.decided_by ? `（${b.decided_by}）` : '（超时）'}`;
      case 'log': return b.message;
      default: return JSON.stringify(b);
    }
  }
</script>

<PageHeader title={task?.name ?? `Run ${runId.slice(0, 8)}`} crumb="执行记录" crumbHref="/runs">
  {#if run}
    <StatusPill status={run.status} />
    {#if run.dry_run}<span class="tag">影子执行</span>{/if}
    <!-- 连接状态要一直可见：断开时看到的是一份不再更新的快照，
         不标出来的话人会以为"这个 run 卡住了" -->
    {#if !isTerminal(run.status)}
      <span class="tag" class:accent={status === 'open'}>
        {status === 'open' ? '实时' : status === 'connecting' ? '连接中…' : '已断开'}
      </span>
    {/if}
  {/if}
  {#snippet sub()}
    {#if run}
      <span>{triggerLabel(run.trigger)}触发</span>
      <span title="入队 {stamp(run.created_at)}（{DISPLAY_TIMEZONE}）">开始 {run.started_at ? stamp(run.started_at) : '—'}</span>
      <span>耗时 {elapsed}</span>
      <span>花费 {money(cost)}</span>
      <span>{events.length} 条事件</span>
      <button class="idbtn mono faint" title="复制完整 id" onclick={copyId}>{runId.slice(0, 8)}</button>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if run}
      <Dropdown label="更多" disabled={busy}>
        <a href="/tasks/{run.task_id}" role="menuitem">看任务定义</a>
        <button onclick={again}>
          再跑一次
          <span class="hint">{run.dry_run ? '同样是影子执行' : '同一个任务，新的一次 run'}</span>
        </button>
        {#if isTerminal(run.status)}
          <hr />
          <button class="danger" onclick={() => (confirmingDelete = true)}>
            删除这条记录
            <span class="hint">连同事件流和资源采样</span>
          </button>
        {/if}
      </Dropdown>
      {#if !isTerminal(run.status)}
        <button class="btn-danger" onclick={() => (confirmingCancel = true)} disabled={busy}>取消执行</button>
      {/if}
    {/if}
  {/snippet}
</PageHeader>

<Confirm bind:open={confirmingCancel} title="取消这次执行？" danger confirmText="取消执行" {busy} onconfirm={cancel}>
  <p>正在跑的步骤会被中断。已经落地的改动不会回滚，已经花掉的钱也不会退。</p>
</Confirm>

<Confirm bind:open={confirmingDelete} title="删除这次执行记录？" danger confirmText="删除" {busy} onconfirm={removeRun}>
  <p>
    连同它的 {events.length} 条事件和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。
  </p>
  <p class="warn-text">删掉之后拿不回来。</p>
</Confirm>

{#if error}<div class="banner">{error}</div>{/if}

{#if run?.error}
  <div class="callout danger">
    <strong>{run.status === 'cancelled' ? '已取消' : '失败原因'}：</strong>{run.error}
  </div>
{/if}

{#if pending.length}
  <section class="gate">
    {#each pending as approval (approval.id)}
      <ApprovalCard {approval} ondecided={loadApprovals} />
    {/each}
  </section>
{/if}

<Process {events} spec={task?.spec ?? null} {hosts} />

{#if events.length === 0}
  <section class="card waiting">
    <span class="dot running"></span>
    <span>{status === 'open' ? '已连上，等第一条事件…' : status === 'connecting' ? '正在连接事件流…' : '没有收到任何事件'}</span>
  </section>
{/if}

<ResourceChart {runId} revision={metricsRevision} {degraded} />
<DriftPanel {runId} revision={metricsRevision} />

<details class="events-block">
  <summary>
    <svg viewBox="0 0 24 24" class="chev"><path d="M9 6l6 6-6 6" /></svg>
    <h2>原始事件流</h2>
    <span class="faint">{events.length} 条 · 按 seq 排、不合并、不过滤。排查用。</span>
  </summary>
  <div class="events-tools">
    <input bind:value={find} placeholder="在事件里找：kind、节点、文本" type="search" spellcheck="false" />
    {#if find}<span class="faint small">匹配 {shown.length} 条</span>{/if}
  </div>
  <ol class="events">
    {#each shown as event (event.seq)}
      <li
        class="{event.body.kind} {event.body.kind === 'policy_decided'
          ? `effect-${event.body.effect}`
          : ''}"
      >
        <span class="seq mono">{event.seq}</span>
        <time class="mono" title={stamp(event.ts)}>{clock(event.ts)}</time>
        <span class="kind">{event.body.kind}</span>
        <span class="node mono">{event.node_key ?? ''}</span>
        <span class="text">{summarize(event)}</span>
      </li>
    {:else}
      <li class="none faint">{find ? '没有匹配的事件' : '等待事件…'}</li>
    {/each}
  </ol>
</details>

<style>
  .idbtn {
    border: none;
    background: none;
    padding: 0;
    font-size: 0.8rem;
  }
  .idbtn:hover {
    color: var(--accent-fg);
    background: none;
  }
  .callout {
    margin-bottom: var(--s4);
    overflow-wrap: anywhere;
  }
  .gate {
    margin-bottom: var(--s4);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .waiting {
    display: flex;
    align-items: center;
    gap: var(--s3);
    color: var(--fg-dim);
    font-size: 0.85rem;
    margin-top: var(--s4);
  }

  .events-block {
    margin-top: var(--s5);
  }
  .events-block summary {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    flex-wrap: wrap;
    margin-bottom: var(--s3);
  }
  .events-block h2 {
    margin: 0;
    display: inline;
  }
  .chev {
    width: 14px;
    height: 14px;
    align-self: center;
    fill: none;
    stroke: var(--fg-faint);
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform 0.12s ease;
  }
  .events-block[open] .chev {
    transform: rotate(90deg);
  }
  .events-tools {
    display: flex;
    align-items: center;
    gap: var(--s3);
    margin-bottom: var(--s2);
  }
  .events-tools input {
    width: min(26rem, 100%);
  }
  .events {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: var(--r3);
    overflow: hidden;
    background: var(--surface-1);
  }
  .events li {
    display: grid;
    grid-template-columns: 3rem 4.5rem 9rem 6rem 1fr;
    gap: var(--s3);
    align-items: baseline;
    padding: 0.35rem var(--s4);
    font-size: 0.83rem;
    /* 新事件从上方滑入。实时流里"有新东西来了"要看得见 */
    animation: slide-in 0.18s ease-out;
  }
  @keyframes slide-in {
    from {
      opacity: 0;
      transform: translateY(-3px);
    }
  }
  .events li + li {
    border-top: 1px solid var(--line);
  }
  .events li:hover {
    background: var(--surface-2);
  }
  .seq {
    color: var(--fg-faint);
    font-size: 0.75rem;
    text-align: right;
  }
  time {
    color: var(--fg-faint);
    font-size: 0.75rem;
  }
  .kind {
    color: var(--fg-faint);
    font-size: 0.75rem;
  }
  .node {
    color: var(--fg-dim);
    font-size: 0.75rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .text {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--fg-dim);
  }
  .none {
    display: block !important;
    padding: var(--s5) var(--s4);
    text-align: center;
  }

  /* 需要被看见的几类事件。其余保持低对比，免得整屏都在喊 */
  .events li.agent_text .text {
    color: var(--fg);
  }
  .events li.run_finished .text,
  .events li.node_finished .text {
    color: var(--fg);
  }
  .events li.drift_detected .text,
  .events li.resource_degraded .text {
    color: var(--warn);
  }
  /* 放行的判决不该是红的——审计里 allow 和 deny 一样多，全红就没有信号了 */
  .events li.effect-deny .text,
  .events li.effect-ask .text {
    color: var(--bad);
  }
  .events li.effect-deny {
    background: color-mix(in srgb, var(--bad) 5%, transparent);
  }
  .events li.effect-allow .kind {
    color: var(--ok);
  }

  @media (max-width: 900px) {
    .events li {
      grid-template-columns: 2.5rem 1fr;
    }
    .events li time,
    .events li .kind,
    .events li .node {
      display: none;
    }
  }
</style>
