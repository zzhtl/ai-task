<script lang="ts">
  import { page } from '$app/state';
  import { api, ApiFailure } from '$api/client';
  import { subscribeRunEvents, type EventStream } from '$api/events';
  import { cancelRun, getRun } from '$api/runs';
  import type { NodeStatus } from '$api/types/NodeStatus';
  import type { RunEvent } from '$api/types/RunEvent';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskDetail } from '$api/types/TaskDetail';
  import DagCanvas from '$lib/dag/DagCanvas.svelte';
  import ResourceChart from '$lib/metrics/ResourceChart.svelte';
  import DriftPanel from '$lib/drift/DriftPanel.svelte';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';

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

  const shown = $derived(events.filter((e) => e.body.kind !== 'agent_thinking'));

  // 编排图 + 实时状态叠在同一张图上：看一个正在跑的 run 时，
  // 不用在"图"和"事件流"之间来回对照。
  let task = $state<TaskDetail | null>(null);
  $effect(() => {
    if (!run?.task_id) return;
    api<TaskDetail>(`/api/v1/tasks/${run.task_id}`).then((t) => (task = t)).catch(() => {});
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

<header>
  <div>
    <a href="/" class="muted">← 返回</a>
    <h1>Run <span class="mono">{runId.slice(0, 8)}</span></h1>
    {#if run}
      <p class="sub">
        <span class="status {run.status}">{run.status}</span>
        · ${cost} · {events.length} 事件
        <span class="conn {status}">{status === 'open' ? '实时' : status === 'connecting' ? '连接中' : '已结束'}</span>
      </p>
    {/if}
  </div>
  {#if run && !['succeeded', 'failed', 'cancelled', 'timed_out', 'budget_exceeded', 'resource_exceeded'].includes(run.status)}
    <button onclick={() => cancelRun(runId).catch((e) => (error = String(e)))}>取消</button>
  {/if}
</header>

{#if error}<p class="bad">{error}</p>{/if}

{#if task}
  <DagCanvas spec={task.spec} status={nodeStatus} {expanded} />
{/if}

{#if pending.length}
  <section class="gate">
    <h2>这个 run 正在等人点头</h2>
    {#each pending as approval (approval.id)}
      <ApprovalCard {approval} ondecided={loadApprovals} />
    {/each}
  </section>
{/if}

<ResourceChart {runId} revision={metricsRevision} {degraded} />
<DriftPanel {runId} revision={metricsRevision} />

<ol class="events">
  {#each shown as event (event.seq)}
    <li class="{event.body.kind} {event.body.kind === 'policy_decided'
      ? `effect-${event.body.effect}`
      : ''}">
      <span class="seq mono">{event.seq}</span>
      <span class="kind">{event.body.kind}</span>
      {#if event.node_key}<span class="node">{event.node_key}</span>{/if}
      <span class="text">{summarize(event)}</span>
    </li>
  {:else}
    <li class="muted">等待事件…</li>
  {/each}
</ol>

<style>
  header { display: flex; align-items: flex-start; justify-content: space-between; }
  h1 { margin: 0.5rem 0 0; font-size: 1.4rem; }
  .sub { margin: 0.25rem 0 0; color: var(--muted); }
  .conn { margin-left: 0.75rem; font-size: 0.8rem; }
  .conn.open { color: var(--ok); }
  .events { list-style: none; margin: 1.25rem 0 0; padding: 0; border: 1px solid var(--line); border-radius: 0.75rem; overflow: hidden; }
  .events li { display: grid; grid-template-columns: 3rem 9rem auto 1fr; gap: 0.75rem; align-items: baseline; padding: 0.4rem 0.9rem; background: var(--card); }
  .events li + li { border-top: 1px solid var(--line); }
  .seq { color: var(--muted); font-size: 0.8rem; text-align: right; }
  .kind { color: var(--muted); font-size: 0.8rem; }
  .node { font-size: 0.8rem; opacity: 0.8; }
  .text { white-space: pre-wrap; overflow-wrap: anywhere; }
  .events li.drift_detected .text { color: var(--bad); font-weight: 500; }
  /* 放行的判决不该是红的——审计里 allow 和 deny 一样多，全红就没有信号了 */
  .events li.effect-deny .text,
  .events li.effect-ask .text { color: var(--bad); }
  .events li.effect-allow .kind { color: var(--ok); }
  .events li.agent_text .text { font-weight: 500; }
  .gate { margin-top: 1.25rem; display: flex; flex-direction: column; gap: 0.6rem; }
  .gate h2 { font-size: 0.95rem; margin: 0; }
</style>
