<script lang="ts">
  /**
   * 任务详情：这个任务会按什么顺序做什么、什么时候自己跑、最近跑得怎么样。
   *
   * 这里**没有 YAML**。之前这个位置放的是一个假编辑器——一大块 YAML，改完
   * 不保存。它既拦住了看不懂 YAML 的人，又骗了看得懂的人。现在左边是步骤和
   * 执行记录，右边是定时和元信息，要改就去编辑页。
   */
  import { session } from '$lib/auth/session.svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { api, describeError, ignoreForbidden } from '$api/client';
  import { pollWhileVisible } from '$api/resource.svelte';
  import { listRuns, triggerRun, newIdempotencyKey } from '$api/runs';
  import { mergeFirstPage } from '$lib/runs/merge';
  import { listHosts } from '$api/models';
  import type { TaskDetail } from '$api/types/TaskDetail';
  import type { RunSummary } from '$api/types/RunSummary';
  import SchedulePanel from '$lib/schedules/SchedulePanel.svelte';
  import StepList from '$lib/tasks/StepList.svelte';
  import DagView from '$lib/dag/DagView.svelte';
  import { fromSpec } from '$lib/tasks/compose';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Dropdown from '$lib/ui/Dropdown.svelte';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import CopyButton from '$lib/ui/CopyButton.svelte';
  import HelpTip from '$lib/ui/HelpTip.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, duration, money, stamp, triggerLabel } from '$lib/ui/format';

  const taskId = $derived(page.params.id ?? '');

  let task = $state<TaskDetail | null>(null);
  /** viewer 只能看：写操作的入口灰掉，而不是点了再弹 403。 */
  const canOperate = $derived(session.can('operator'));
  let error = $state<string | null>(null);
  let busy = $state(false);
  let hosts = $state<Array<{ id: string; name: string }>>([]);
  let runs = $state<RunSummary[]>([]);
  let runsCursor = $state<string | null>(null);
  let runsLoaded = $state(false);

  async function loadTask() {
    try {
      task = await api<TaskDetail>(`/api/v1/tasks/${taskId}`);
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

  async function moreRuns() {
    if (!runsCursor) return;
    const r = await listRuns({ taskId, limit: 50, cursor: runsCursor });
    runs = [...runs, ...r.items];
    runsCursor = r.next_cursor ?? null;
  }

  $effect(() => {
    if (!taskId) return;
    // 从一个任务直接跳到另一个任务时组件是复用的：不清空的话，
    // 下面的"合并第一页"会把两个任务的执行记录并到一张表里
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

  const stats = $derived.by(() => {
    const done = runs.filter((r) => r.status !== 'queued' && r.status !== 'running');
    const ok = done.filter((r) => r.status === 'succeeded').length;
    const spend = runs.reduce((s, r) => s + Number(r.cost_usd), 0);
    return { total: runs.length, ok, done: done.length, spend };
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

  /**
   * 停用 / 启用。停用的任务：定时到点不触发，手动也触发不了。
   * PUT 是整体替换，所以把现有定义原样带回去，只翻 enabled 这一位。
   */
  async function setEnabled(enabled: boolean) {
    if (!task) return;
    busy = true;
    try {
      await api(`/api/v1/tasks/${taskId}`, {
        method: 'PUT',
        body: {
          name: task.name,
          description: task.description ?? null,
          spec: task.spec,
          rules: task.rules ?? [],
          enabled
        },
        ifMatch: String(task.version)
      });
      toast(enabled ? '任务已启用' : '任务已停用，定时不会再触发');
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
   * 会被牵连的数量摆出来，再让人确认。
   *
   * 数量是**开了弹层之后**再去数的：为了数一下就先卡住几百毫秒不响应，
   * 会让人以为按钮没点上而去点第二次。
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
      await goto('/tasks');
    } catch (e) {
      toastError(describeError(e));
      busy = false;
    }
  }

</script>

<PageHeader title={task?.name ?? '任务'} crumb="任务" crumbHref="/tasks">
  {#if task && !task.enabled}<span class="tag danger">已停用</span>{/if}
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
      <a href="/tasks/new?id={taskId}" role="menuitem">
        编辑定义
        <span class="hint">保存会产生新版本</span>
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
    {#if canOperate}<a class="btn" href="/tasks/new?id={taskId}">编辑</a>{/if}
    <Dropdown label="运行" primary disabled={busy || !task || !task.enabled || !canOperate}>
      <button onclick={() => run(false)}>
        立即执行
        <span class="hint">真的会在目标机上动手</span>
      </button>
      <button onclick={() => run(true)}>
        影子执行
        <span class="hint">只记录意图不落地，用来验证提示词</span>
      </button>
    </Dropdown>
  {/snippet}
</PageHeader>

<Confirm
  open={confirming} onclose={() => (confirming = false)}
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
    <strong>这个任务已停用。</strong>定时到点不会触发，也不能手动运行。
    在右上角「更多」里可以重新启用。
  </div>
{/if}

<div class="layout">
  <div class="main">
    <section class="card">
      <header class="card-head">
        <h2>执行步骤</h2>
        <HelpTip text="从上到下依次执行。每一步能看到它勾选的前几步的结果，那些结果会作为「上游输入」放进提示词。" />
      </header>
      {#if !task}
        <Loading rows={4} />
      {:else if comp}
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
      {:else if task.spec}
        <!-- 步骤列表是一条直线，表示不了分支/并行/map。以前这里只说一句
             "显示不了"，现在把图画出来——编排工具画不出你编排的图是说不过去的。 -->
        <div class="callout">
          这个编排有分支、并行或 map 这类结构，直线的步骤列表表示不了，所以画成图。
          界面上还改不了它——它是通过接口写进来的。
        </div>
        <DagView spec={task.spec} />
      {/if}
    </section>

    <section class="card">
      <header class="card-head">
        <h2>执行记录</h2>
        {#if runs.length}
          <span class="sub">
            最近 {stats.total} 次 · 成功 {stats.ok}/{stats.done} · 花费 {money(stats.spend)}
          </span>
        {/if}
        <span class="spacer"></span>
        <a class="btn btn-ghost btn-sm" href="/runs?task={taskId}">在执行记录页看</a>
      </header>
      {#if !runsLoaded}
        <Loading rows={3} />
      {:else if runs.length}
        <ul class="runs">
          {#each runs as r (r.id)}
            <li>
              <a href="/runs/{r.id}">
                <StatusBadge status={r.status} variant="text" />
                <span class="col">
                  <span class="faint">{triggerLabel(r.trigger)}{r.dry_run ? ' · 影子' : ''}</span>
                  {#if r.error}<span class="err ellipsis" title={r.error}>{r.error}</span>{/if}
                </span>
                <span class="spacer"></span>
                <span class="faint">{duration(r.started_at, r.finished_at)}</span>
                <span class="faint">{money(r.cost_usd)}</span>
                <span class="faint" title={stamp(r.created_at)}>{ago(r.created_at)}</span>
                <span class="mono faint">{r.id.slice(0, 8)}</span>
              </a>
            </li>
          {/each}
        </ul>
        {#if runsCursor}
          <div class="more"><button class="btn-ghost btn-sm" onclick={moreRuns}>更早的记录</button></div>
        {/if}
      {:else}
        <Empty compact title="还没跑过" hint="点右上角「运行」，或者配一条定时。" />
      {/if}
    </section>
  </div>

  <aside class="side">
    {#if taskId}
      <SchedulePanel {taskId} taskEnabled={task?.enabled ?? true} />
    {/if}

    {#if task}
      <section class="card">
        <header class="card-head"><h2>信息</h2></header>
        <dl class="facts">
          <dt>状态</dt>
          <dd>{task.enabled ? '启用' : '已停用'}</dd>
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
          <dt>id</dt>
          <dd><code class="wrap">{task.id}</code></dd>
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
  .budget {
    margin: var(--s2) 0 0;
    padding-left: calc(2rem + var(--s3));
  }
  .runs {
    list-style: none;
    margin: 0 calc(-1 * var(--s2));
    padding: 0;
  }
  .runs a {
    display: flex;
    align-items: center;
    gap: var(--s3);
    padding: 0.45rem var(--s2);
    border-radius: var(--r2);
    font-size: var(--t-base);
    color: inherit;
  }
  .runs a:hover {
    background: var(--surface-2);
  }
  .col {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.35;
  }
  .more {
    display: flex;
    justify-content: center;
    padding-top: var(--s2);
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
  .wrap {
    overflow-wrap: anywhere;
  }
</style>
