<script lang="ts">
  /**
   * 任务详情：这个任务按什么顺序做什么、什么时候自己跑、最近跑得怎么样。
   *
   * 这里**没有 YAML**。之前这个位置放的是一个假编辑器——一大块 YAML，改完
   * 不保存。它既拦住了看不懂 YAML 的人，又骗了看得懂的人。现在主区是步骤和
   * 执行记录，右边是定时、最近表现和元信息，要改就去编辑页。
   */
  import { session } from '$lib/auth/session.svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { api, describeError, ignoreForbidden } from '$api/client';
  import { invalidate, pollWhileVisible } from '$api/resource.svelte';
  import { getTask, listRuns, newIdempotencyKey, setTaskEnabled, triggerRun } from '$api/runs';
  import { mergeFirstPage } from '$lib/runs/merge';
  import { listHosts } from '$api/models';
  import type { TaskDetail } from '$api/types/TaskDetail';
  import type { RunListItem } from '$api/types/RunListItem';
  import SchedulePanel from '$lib/schedules/SchedulePanel.svelte';
  import StepList from '$lib/tasks/StepList.svelte';
  import RunTable from '$lib/runs/RunTable.svelte';
  import DagView from '$lib/dag/DagView.svelte';
  import { fromSpec } from '$lib/tasks/compose';
  import { successOf } from '$lib/tasks/recent';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Tabs from '$lib/ui/Tabs.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Dropdown from '$lib/ui/Dropdown.svelte';
  import SplitButton from '$lib/ui/SplitButton.svelte';
  import CopyButton from '$lib/ui/CopyButton.svelte';
  import HelpTip from '$lib/ui/HelpTip.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, humanDuration, moneyMicros, stamp, toMicros } from '$lib/ui/format';

  const taskId = $derived(page.params.id ?? '');

  let task = $state<TaskDetail | null>(null);
  /** viewer 只能看：写操作的入口灰掉，而不是点了再弹 403。 */
  const canOperate = $derived(session.can('operator'));
  let error = $state<string | null>(null);
  let busy = $state(false);
  let hosts = $state<Array<{ id: string; name: string }>>([]);
  let runs = $state<RunListItem[]>([]);
  let runsCursor = $state<string | null>(null);
  let runsLoaded = $state(false);

  type Tab = 'steps' | 'runs';
  let tab = $state<Tab>('steps');

  async function loadTask() {
    try {
      task = await getTask(taskId);
      error = null;
    } catch (e) {
      error = describeError(e);
    }
  }

  async function loadRuns() {
    try {
      const r = await listRuns({ taskId, limit: 20 });
      // 首次整页放进来；之后的轮询只合并第一页，保住"更早的记录"翻出来的那几页
      if (!runsLoaded) {
        runs = r.items;
        runsCursor = r.next_cursor ?? null;
      } else {
        runs = mergeFirstPage(runs, r.items);
        runsCursor ??= r.next_cursor ?? null;
      }
    } catch {
      /* 执行记录读不到不该把整个任务页弄坏 */
    } finally {
      runsLoaded = true;
    }
  }

  let loadingMore = $state(false);
  async function moreRuns() {
    if (!runsCursor || loadingMore) return;
    loadingMore = true;
    try {
      const r = await listRuns({ taskId, limit: 50, cursor: runsCursor });
      const known = new Set(runs.map((x) => x.id));
      runs = [...runs, ...r.items.filter((x) => !known.has(x.id))];
      runsCursor = r.next_cursor ?? null;
    } catch (e) {
      toastError(describeError(e));
    } finally {
      loadingMore = false;
    }
  }

  $effect(() => {
    if (!taskId) return;
    // 从一个任务直接跳到另一个任务时组件是复用的：不清空的话，
    // 下面的"合并第一页"会把两个任务的执行记录并到一张表里
    task = null;
    runs = [];
    runsCursor = null;
    runsLoaded = false;
    void loadTask();
    void loadRuns();
    // 主机是 admin 才能读；operator 看任务时读不到不该报错
    listHosts()
      .then((h) => (hosts = h))
      .catch(ignoreForbidden);
    // 有 run 在跑时这页就是"看进度"的地方，得自己刷新。
    // 执行记录是游标翻页追加的，用不了通用缓存那套"整份替换"，
    // 但"看不见就别打"这件事可以单独拿过来。
    return pollWhileVisible(() => void loadRuns(), 5000);
  });

  // 步骤列表表示不了的编排（分支、并行、map）现在没有任何界面路径能创建，
  // 但接口收得下。这种情况下老实说"这里显示不了"，别假装。
  const comp = $derived(task ? fromSpec(task.spec) : null);

  /**
   * 目标机 CLI 执行的 AI 步骤不受影子执行保护：它的内置工具不经过策略层，
   * "只记录意图"拦不住它。影子执行旁边得把这句话说出来。
   */
  const hostCliSteps = $derived(
    (task?.spec.nodes ?? []).filter((n) => n.config.kind === 'ai' && n.config.executor === 'host_cli').length
  );

  /** 最近表现只看已经加载的这些（默认最近 20 次），标签上照实写"最近 N 次"。 */
  const perf = $derived.by(() => {
    const rate = successOf(runs);
    const ok = runs.filter((r) => r.status === 'succeeded' && !r.dry_run && r.started_at && r.finished_at);
    const avgMs = ok.length
      ? ok.reduce((sum, r) => sum + Date.parse(r.finished_at!) - Date.parse(r.started_at!), 0) / ok.length
      : null;
    const spend = runs.reduce((sum, r) => sum + toMicros(r.cost_usd), 0);
    const lastOk = runs.find((r) => r.status === 'succeeded' && !r.dry_run) ?? null;
    return { rate, avgMs, spend, lastOk, count: runs.length };
  });

  /** 正式执行和影子执行是两种意图，各自一个幂等键，成功后才丢。 */
  const triggerKeys = new Map<string, string>();

  async function run(dryRun: boolean) {
    busy = true;
    const slot = dryRun ? 'dry' : 'live';
    const key = triggerKeys.get(slot) ?? newIdempotencyKey();
    triggerKeys.set(slot, key);
    try {
      const r = await triggerRun(taskId, { dry_run: dryRun }, key);
      triggerKeys.delete(slot);
      toast(dryRun ? '已触发影子执行' : '已触发执行');
      await goto(`/runs/${r.id}`);
    } catch (e) {
      toastError(describeError(e));
      busy = false;
    }
  }

  async function setEnabled(enabled: boolean) {
    busy = true;
    try {
      await setTaskEnabled(taskId, enabled);
      toast(enabled ? '任务已启用' : '任务已停用，定时不会再触发');
      invalidate('tasks');
      await loadTask();
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = false;
    }
  }

  /**
   * 删任务。**级联删掉它的全部执行历史。**
   *
   * 那些 run 里有成本记录和完整事件流，是审计材料，没有撤销键——所以先把
   * 会被牵连的数量摆出来，再让人确认。数量是开了弹层之后再去数的。
   */
  let confirming = $state(false);
  /** `null` = 还在数；`-1` = 数不出来。 */
  let affectedRuns = $state<number | null>(null);

  function askDelete() {
    confirming = true;
    affectedRuns = null;
    void listRuns({ taskId, limit: 200 })
      .then((p) => (affectedRuns = p.items.length + (p.next_cursor ? 1 : 0)))
      .catch(() => (affectedRuns = -1));
  }

  async function remove() {
    busy = true;
    try {
      await api(`/api/v1/tasks/${taskId}`, { method: 'DELETE' });
      toast(`已删除「${task?.name ?? ''}」`);
      invalidate('tasks', 'schedules');
      await goto('/tasks');
    } catch (e) {
      toastError(describeError(e));
      busy = false;
    }
  }
</script>

<PageHeader title={task?.name ?? '任务'} crumb="任务" crumbHref="/tasks">
  {#if task && !task.enabled}<span class="tag">已停用</span>{/if}
  {#snippet sub()}
    {#if task}
      {#if task.description}<span class="desc">{task.description}</span>{/if}
      <span>v{task.version_no}</span>
      <span>{task.spec.nodes.length} 个步骤</span>
      <span><CopyButton text={taskId} display={taskId.slice(0, 8)} label="复制任务 id" /></span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <Dropdown label="更多" disabled={busy || !task || !canOperate}>
      <a href="/tasks/new?from={taskId}">
        复制为新任务
        <span class="hint">步骤和规则照搬，另起一个名字</span>
      </a>
      {#if task?.enabled}
        <button onclick={() => setEnabled(false)}>
          停用任务
          <span class="hint">定时不再触发，也不能手动运行</span>
        </button>
      {:else}
        <button onclick={() => setEnabled(true)}>启用任务</button>
      {/if}
      <hr />
      <button class="danger" onclick={askDelete}>
        删除任务
        <span class="hint">连同全部执行记录</span>
      </button>
    </Dropdown>
    {#if canOperate}<a class="btn" href="/tasks/new?id={taskId}"><Icon name="pencil" size={14} />编辑</a>{/if}
    <SplitButton
      label="运行"
      menuLabel="更多运行方式"
      disabled={busy || !task || !task.enabled || !canOperate}
      title={!canOperate ? '需要 operator 权限' : task?.enabled === false ? '任务已停用，先启用' : '立即执行一次，真的会在目标机上动手'}
      onclick={() => run(false)}
    >
      <button onclick={() => run(true)}>
        影子执行
        <span class="hint">只记录意图不落地，用来试提示词</span>
        {#if hostCliSteps}
          <span class="hint warn-text">
            有 {hostCliSteps} 步用目标机上的 CLI 执行，它的内置工具不经过策略层，影子执行拦不住
          </span>
        {/if}
      </button>
    </SplitButton>
  {/snippet}
</PageHeader>

<Confirm
  open={confirming}
  onclose={() => (confirming = false)}
  title="删除任务「{task?.name ?? ''}」？"
  danger
  confirmText="删除"
  {busy}
  onconfirm={remove}
>
  {#if affectedRuns === null}
    <p>正在数有多少条执行记录会被牵连…</p>
  {:else if affectedRuns > 0}
    <p>
      会同时删掉 <b>{affectedRuns > 200 ? '200 条以上' : `${affectedRuns} 次`}</b>
      执行记录和它们的完整事件流。那里面有成本记录和策略判决，是审计材料。
    </p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {:else if affectedRuns < 0}
    <p>数不出有多少执行记录，但它们会跟着一起删掉。</p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {:else}
    <p>这个任务还没有执行记录。删掉之后拿不回来。</p>
  {/if}
</Confirm>

{#if error}<div class="banner">{error}</div>{/if}

{#if task && !task.enabled}
  <div class="callout warn">
    <strong>这个任务已停用。</strong>定时到点不会触发，也不能手动运行。在右上角「更多」里可以重新启用。
  </div>
{/if}

<div class="layout">
  <div class="main">
    <Tabs
      tabs={[
        { id: 'steps', label: '步骤' },
        { id: 'runs', label: '执行记录' }
      ]}
      bind:value={tab}
    />

    {#if tab === 'steps'}
      <section class="card">
        {#if !task}
          <Loading rows={4} />
        {:else if comp}
          <p class="lead-line">
            从上到下依次执行
            <HelpTip text="每一步能看到它勾选的前几步的结果，那些结果会作为「上游输入」放进提示词。" />
          </p>
          <StepList {comp} {hosts} />
          <details class="graph">
            <summary class="faint small">看编排图</summary>
            <DagView spec={task.spec} />
          </details>
          {#if comp.budgetUsd}
            <p class="faint small budget">
              花费上限 <b>${comp.budgetUsd}</b>，累计到这个数就不再启动新步骤。
            </p>
          {/if}
        {:else}
          <!-- 步骤列表是一条直线，表示不了分支/并行/map。编排工具画不出你编排的图是说不过去的 -->
          <div class="callout">
            这个编排有分支、并行或 map 这类结构，直线的步骤列表表示不了，所以画成图。
            界面上还改不了它——它是通过接口写进来的。
          </div>
          <DagView spec={task.spec} />
        {/if}
      </section>
    {:else}
      <section class="card flush">
        {#if !runsLoaded}
          <div class="pad"><Loading rows={3} /></div>
        {:else if runs.length}
          <RunTable {runs} showTask={false} onremoved={(id) => (runs = runs.filter((r) => r.id !== id))} />
          <div class="more">
            {#if runsCursor}
              <button class="btn-ghost btn-sm" onclick={moreRuns} disabled={loadingMore}>
                {loadingMore ? '加载中…' : '加载更早的记录'}
              </button>
            {/if}
            <a class="btn btn-ghost btn-sm" href="/runs?task={taskId}">到执行记录页筛选</a>
          </div>
        {:else}
          <Empty compact title="还没跑过" hint="点右上角「运行」，或者配一条定时。" />
        {/if}
      </section>
    {/if}
  </div>

  <aside class="side">
    {#if taskId}
      <SchedulePanel {taskId} taskEnabled={task?.enabled ?? true} />
    {/if}

    <section class="card">
      <header class="card-head">
        <h2>最近表现</h2>
        {#if perf.count}<span class="sub">最近 {perf.count} 次</span>{/if}
      </header>
      {#if !runsLoaded}
        <Loading rows={2} />
      {:else if perf.count}
        <dl class="facts">
          <dt>成功</dt>
          <dd>
            {#if perf.rate.done}
              <b class:low={perf.rate.ok < perf.rate.done}>{perf.rate.ok}/{perf.rate.done}</b>
              <span class="faint">（不算影子、进行中和取消的）</span>
            {:else}
              <span class="faint">还没有跑出结论的</span>
            {/if}
          </dd>
          <dt>成功时耗时</dt>
          <dd>{perf.avgMs === null ? '—' : `平均 ${humanDuration(perf.avgMs)}`}</dd>
          <dt>花费</dt>
          <dd>{moneyMicros(perf.spend)}</dd>
          <dt>上次成功</dt>
          <dd>
            {#if perf.lastOk}
              <a href="/runs/{perf.lastOk.id}" title={stamp(perf.lastOk.created_at)}>{ago(perf.lastOk.created_at)}</a>
            {:else}
              <span class="faint">—</span>
            {/if}
          </dd>
        </dl>
      {:else}
        <p class="faint small">还没跑过。</p>
      {/if}
    </section>

    {#if task}
      <section class="card">
        <header class="card-head"><h2>信息</h2></header>
        <dl class="facts">
          <dt>定义版本</dt>
          <dd>v{task.version_no}<span class="faint">（历史 run 绑的是各自的版本快照）</span></dd>
          <dt>规则</dt>
          <dd>
            {#if task.rules?.length}
              {#each task.rules as r (r)}<span class="tag">{r}</span>{/each}
            {:else}
              <span class="faint">只有全局规则</span>
            {/if}
          </dd>
          <dt>花费上限</dt>
          <dd>{task.spec.budget_usd ? `$${task.spec.budget_usd}` : '不设'}</dd>
          <dt>更新</dt>
          <dd title={stamp(task.updated_at)}>{ago(task.updated_at)}</dd>
          <dt>创建</dt>
          <dd>{stamp(task.created_at)}</dd>
        </dl>
      </section>
    {/if}
  </aside>
</div>

<style>
  .graph {
    margin-top: var(--s3);
  }
  .graph > summary {
    padding: var(--s1) 0;
  }
  .desc {
    color: var(--fg);
  }
  .callout {
    margin-bottom: var(--s4);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 340px;
    gap: var(--s4);
    align-items: start;
  }
  @media (max-width: 1280px) {
    .layout {
      grid-template-columns: 1fr;
    }
  }
  .main,
  .side {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    min-width: 0;
  }
  .lead-line {
    margin: 0 0 var(--s3);
    display: flex;
    align-items: center;
    gap: var(--s1);
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .budget {
    margin: var(--s2) 0 0;
    padding-left: calc(2rem + var(--s3));
  }
  .pad {
    padding: var(--s4);
  }
  .more {
    display: flex;
    justify-content: center;
    gap: var(--s2);
    padding: var(--s2);
    border-top: 1px solid var(--line);
  }
  .facts {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.4rem var(--s3);
    margin: 0;
    font-size: var(--t-sm);
  }
  .facts dt {
    color: var(--fg-faint);
    white-space: nowrap;
  }
  .facts dd {
    margin: 0;
    min-width: 0;
    display: flex;
    gap: var(--s1);
    flex-wrap: wrap;
    align-items: center;
  }
  .facts b.low {
    color: var(--bad-fg);
  }
</style>
