<script lang="ts">
  import { page } from '$app/state';
  import { api, ApiFailure } from '$api/client';
  import { subscribeRunEvents, type EventStream } from '$api/events';
  import { cancelRun, getRun } from '$api/runs';
  import type { NodeStatus } from '$api/types/NodeStatus';
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
  import { duration, money, stamp } from '$lib/ui/format';

  const runId = $derived(page.params.id ?? '');

  let run = $state<RunSummary | null>(null);
  let events = $state<RunEvent[]>([]);
  let status = $state<'connecting' | 'open' | 'closed'>('connecting');
  let error = $state<string | null>(null);
  let stream: EventStream | null = null;

  // 终态到了就把 run 概要刷新一次，拿到最终成本和耗时
  $effect(() => {
    if (!runId) return;
    getRun(runId)
      .then((r) => (run = r))
      .catch((e) => (error = e instanceof ApiFailure ? `[${e.code}] ${e.message}` : String(e)));

    stream = subscribeRunEvents(runId, {
      onEvent(event) {
        // 服务端保证 seq 连续，直接追加即可；断线重连也不会重复
        events = [...events, event];
        if (event.body.kind === 'run_finished') {
          getRun(runId).then((r) => (run = r)).catch(() => {});
        }
      },
      onStatus: (s) => (status = s)
    });

    return () => stream?.close();
  });

  // 原始事件流是排查用的，**不过滤**：过滤掉的那类事件恰恰是出问题时想看的。
  // 给人读的版本在「执行过程」里。
  const shown = $derived(events);

  // 编排图 + 实时状态叠在同一张图上：看一个正在跑的 run 时，
  // 不用在"图"和"事件流"之间来回对照。
  let task = $state<TaskDetail | null>(null);
  let hosts = $state<Array<{ id: string; name: string }>>([]);
  $effect(() => {
    if (!run?.task_id) return;
    api<TaskDetail>(`/api/v1/tasks/${run.task_id}`).then((t) => (task = t)).catch(() => {});
  });
  $effect(() => {
    // 主机是 admin 才能读；读不到就在过程里显示短 id，不该报错
    api<{ items: Array<{ id: string; name: string }> }>('/api/v1/hosts')
      .then((p) => (hosts = p.items))
      .catch(() => {});
  });

  const nodeStatus = $derived.by(() => {
    const status: Record<string, NodeStatus> = {};
    for (const event of events) {
      const key = event.node_key;
      if (!key) continue;
      switch (event.body.kind) {
        case 'node_ready': status[key] = 'ready'; break;
        case 'node_started': status[key] = 'running'; break;
        case 'node_retrying': status[key] = 'ready'; break;
        case 'node_finished': status[key] = event.body.status; break;
        case 'node_skipped': status[key] = 'skipped'; break;
        default: break;
      }
    }
    return status;
  });

  const expanded = $derived.by(() => {
    const counts: Record<string, number> = {};
    for (const event of events) {
      if (event.body.kind === 'map_expanded' && event.node_key) {
        counts[event.node_key] = event.body.count;
      }
    }
    return counts;
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
  let pending = $state<
    Array<{
      id: string;
      run_id: string;
      node_key: string | null;
      title: string;
      intent: Record<string, unknown>;
      rule_id: string | null;
      requested_at: string;
      expires_at: string;
      expires_in_s: number;
    }>
  >([]);

  /** 删这次执行记录。连事件流和资源采样一起，不可恢复。 */
  let confirming = $state(false);
  async function removeRun() {
    try {
      await api(`/api/v1/runs/${runId}`, { method: 'DELETE' });
      location.href = '/runs';
    } catch (e) {
      error = String(e);
    }
  }

  async function loadApprovals() {
    try {
      const page = await api<{ items: typeof pending }>('/api/v1/approvals');
      pending = page.items.filter((a) => a.run_id === runId);
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

  /** 未识别的事件类型降级成一行原始 JSON，不能崩也不能静默丢弃。 */
  function summarize(event: RunEvent): string {
    const b = event.body;
    switch (b.kind) {
      case 'run_queued': return `已入队（${b.trigger}${b.dry_run ? '，影子执行' : ''}）`;
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

<PageHeader title="Run {runId.slice(0, 8)}" crumb="← 执行记录" crumbHref="/runs">
  {#snippet sub()}
    {#if run}
      <StatusPill status={run.status} />
      <span>{money(cost)}</span>
      <span>{duration(run.started_at, run.finished_at)}</span>
      <span>{events.length} 事件</span>
      {#if run.dry_run}<span class="tag">影子执行</span>{/if}
      <!-- 连接状态要一直可见：断开时看到的是一份不再更新的快照，
           不标出来的话人会以为"这个 run 卡住了" -->
      <span class="tag" class:accent={status === 'open'}>
        {status === 'open' ? '实时' : status === 'connecting' ? '连接中' : '已结束'}
      </span>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if run && !['succeeded', 'failed', 'cancelled', 'timed_out', 'budget_exceeded', 'resource_exceeded'].includes(run.status)}
      <button class="btn-danger" onclick={() => cancelRun(runId).catch((e) => (error = String(e)))}>
        取消执行
      </button>
    {/if}
    {#if run}<a class="btn" href="/tasks/{run.task_id}">任务定义</a>{/if}
    {#if run?.status && !['running', 'queued'].includes(run.status)}
      <button class="btn-danger" onclick={() => (confirming = true)}>删除</button>
    {/if}
  {/snippet}
</PageHeader>

<Confirm bind:open={confirming} title="删除这次执行记录？" danger confirmText="删除" onconfirm={removeRun}>
  <p>
    连同它的 {events.length} 条事件和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。
  </p>
  <p class="warn-line">删掉之后拿不回来。</p>
</Confirm>

{#if error}<p class="bad">{error}</p>{/if}

{#if pending.length}
  <section class="gate">
    {#each pending as approval (approval.id)}
      <ApprovalCard {approval} ondecided={loadApprovals} />
    {/each}
  </section>
{/if}

<Process {events} spec={task?.spec ?? null} {hosts} />

<ResourceChart {runId} revision={metricsRevision} {degraded} />
<DriftPanel {runId} revision={metricsRevision} />

<details class="events-block">
  <summary>
    <h2>原始事件流 <span class="faint">{shown.length}</span></h2>
    <span class="faint">按 seq 排、不合并、不过滤。排查用。</span>
  </summary>
  <ol class="events">
    {#each shown as event (event.seq)}
      <li
        class="{event.body.kind} {event.body.kind === 'policy_decided'
          ? `effect-${event.body.effect}`
          : ''}"
      >
        <span class="seq mono">{event.seq}</span>
        <time class="mono" title={event.ts}>{stamp(event.ts).slice(11)}</time>
        <span class="kind">{event.body.kind}</span>
        <span class="node mono">{event.node_key ?? ''}</span>
        <span class="text">{summarize(event)}</span>
      </li>
    {:else}
      <li class="waiting faint">等待事件…</li>
    {/each}
  </ol>
</details>

<style>
  .warn-line { color: var(--warn); margin-bottom: 0; }
  .gate { margin-bottom: var(--s4); display: flex; flex-direction: column; gap: var(--s2); }

  .events-block { margin-top: var(--s5); }
  .events-block summary {
    cursor: pointer;
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    flex-wrap: wrap;
    margin-bottom: var(--s2);
  }
  .events-block h2 { margin: 0; display: inline; }
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
    from { opacity: 0; transform: translateY(-3px); }
  }
  .events li + li { border-top: 1px solid var(--line); }
  .events li:hover { background: var(--surface-2); }
  .seq { color: var(--fg-faint); font-size: 0.75rem; text-align: right; }
  time { color: var(--fg-faint); font-size: 0.75rem; }
  .kind { color: var(--fg-faint); font-size: 0.75rem; }
  .node { color: var(--fg-dim); font-size: 0.75rem; overflow: hidden; text-overflow: ellipsis; }
  .text { white-space: pre-wrap; overflow-wrap: anywhere; color: var(--fg-dim); }
  .waiting { display: block !important; padding: var(--s5) var(--s4); text-align: center; }

  /* 需要被看见的几类事件。其余保持低对比，免得整屏都在喊 */
  .events li.agent_text .text { color: var(--fg); }
  .events li.run_finished .text,
  .events li.node_finished .text { color: var(--fg); }
  .events li.drift_detected .text,
  .events li.resource_degraded .text { color: var(--warn); }
  /* 放行的判决不该是红的——审计里 allow 和 deny 一样多，全红就没有信号了 */
  .events li.effect-deny .text,
  .events li.effect-ask .text { color: var(--bad); }
  .events li.effect-deny { background: color-mix(in srgb, var(--bad) 5%, transparent); }
  .events li.effect-allow .kind { color: var(--ok); }

  @media (max-width: 900px) {
    .events li { grid-template-columns: 2.5rem 1fr; }
    .events li time, .events li .kind, .events li .node { display: none; }
  }
</style>
