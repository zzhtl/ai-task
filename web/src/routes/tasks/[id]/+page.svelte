<script lang="ts">
  /**
   * 任务详情：这个任务会按什么顺序做什么。
   *
   * 这里**没有 YAML**。之前这个位置放的是一个假编辑器——一大块 YAML，改完
   * 不保存，底下用小字写着"改动只影响画布预览"。它既拦住了看不懂 YAML 的人，
   * 又骗了看得懂的人。现在上面是图，下面是步骤，要改就去编辑页。
   */
  import { page } from '$app/state';
  import { api, ApiFailure } from '$api/client';
  import { triggerRun } from '$api/runs';
  import type { TaskDetail } from '$api/types/TaskDetail';
  import SchedulePanel from '$lib/schedules/SchedulePanel.svelte';
  import StepList from '$lib/tasks/StepList.svelte';
  import { fromSpec } from '$lib/tasks/compose';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';

  const taskId = $derived(page.params.id ?? '');

  let task = $state<TaskDetail | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let hosts = $state<Array<{ id: string; name: string }>>([]);

  $effect(() => {
    if (!taskId) return;
    api<TaskDetail>(`/api/v1/tasks/${taskId}`)
      .then((t) => (task = t))
      .catch((e) => (error = describe(e)));
    // 主机是 admin 才能读；operator 看任务时读不到不该报错
    api<{ items: Array<{ id: string; name: string }> }>('/api/v1/hosts')
      .then((p) => (hosts = p.items))
      .catch(() => {});
  });

  // 步骤列表表示不了的编排（分支、并行、map）现在没有任何界面路径能创建，
  // 但接口收得下。这种情况下老实说"这里显示不了"，别假装。
  const comp = $derived(task ? fromSpec(task.spec) : null);

  function describe(e: unknown): string {
    return e instanceof ApiFailure ? `[${e.code}] ${e.message}` : String(e);
  }

  async function run() {
    busy = true;
    error = null;
    try {
      const r = await triggerRun(taskId);
      window.location.href = `/runs/${r.id}`;
    } catch (e) {
      error = describe(e);
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
    void api<{ items: unknown[] }>(`/api/v1/runs?task_id=${taskId}&limit=1000`)
      .then((p) => (affectedRuns = p.items.length))
      .catch(() => (affectedRuns = -1));
  }

  async function remove() {
    busy = true;
    try {
      await api(`/api/v1/tasks/${taskId}`, { method: 'DELETE' });
      window.location.href = '/tasks';
    } catch (e) {
      error = describe(e);
      busy = false;
    }
  }
</script>

<PageHeader title={task?.name ?? '任务'} crumb="← 任务" crumbHref="/tasks">
  {#snippet sub()}
    {#if task}
      <span>v{task.version_no}</span>
      <span>{task.spec.nodes.length} 个步骤</span>
      {#if task.rules?.length}<span>规则 {task.rules.join('、')}</span>{/if}
      <span class="mono faint">{taskId.slice(0, 8)}</span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <button class="btn-danger" onclick={askDelete} disabled={busy || !task}>删除</button>
    <a class="btn" href="/tasks/new?id={taskId}">编辑</a>
    <button class="btn-primary" onclick={run} disabled={busy || !task}>运行</button>
  {/snippet}
</PageHeader>

<Confirm
  bind:open={confirming}
  title="删除任务「{task?.name ?? ''}」？"
  danger
  confirmText="删除"
  onconfirm={remove}
>
  {#if affectedRuns === null}
    <p>正在数有多少条执行记录会被牵连…</p>
  {:else if affectedRuns > 0}
    <p>
      会同时删掉 <b>{affectedRuns}</b> 次执行记录和它们的完整事件流。
      那里面有成本记录和策略判决，是审计材料。
    </p>
    <p class="warn-line">删掉之后拿不回来。</p>
  {:else if affectedRuns < 0}
    <p>数不出有多少执行记录，但它们会跟着一起删掉。</p>
    <p class="warn-line">删掉之后拿不回来。</p>
  {:else}
    <p>这个任务还没有执行记录。删掉之后拿不回来。</p>
  {/if}
</Confirm>

{#if error}<p class="bad">{error}</p>{/if}

{#if taskId}
  <SchedulePanel {taskId} />
{/if}

{#if task}
  <section class="steps-block">
    <header>
      <h2>执行步骤</h2>
      <span class="faint">从上到下依次执行，每一步都能看到上一步的结果</span>
    </header>
    {#if comp}
      <StepList {comp} {hosts} />
      {#if comp.budgetUsd}
        <p class="faint budget">
          花费上限 <b>${comp.budgetUsd}</b>，累计到这个数就不再启动新步骤。
        </p>
      {/if}
    {:else}
      <p class="faint">
        这个任务的编排里有分支、并行或 map 这类结构，步骤列表是一条直线，表示不了。
        上面的图是完整的；节点细节点图上的节点看。
      </p>
    {/if}
  </section>
{/if}

<style>
  .warn-line {
    color: var(--warn);
    margin-bottom: 0;
  }
  .steps-block {
    margin-top: var(--s5);
  }
  .steps-block header {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    flex-wrap: wrap;
    margin-bottom: var(--s3);
  }
  .steps-block h2 {
    margin: 0;
  }
  .faint {
    font-size: 0.78rem;
  }
  .budget {
    margin: 0;
    padding-left: calc(2rem + var(--s3));
  }
</style>
