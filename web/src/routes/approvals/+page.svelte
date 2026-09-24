<script lang="ts">
  /**
   * 待审批。
   *
   * 审批是"人正坐在那里等"的场景，但决策可能来自别人。5 秒刷一次够了，
   * 为它开一条 SSE 通道不值得。
   */
  import { describeError, ignoreForbidden } from '$api/client';
  import { resource, invalidate } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import { listRuns, listAllTasks } from '$api/runs';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';

  // 和侧栏徽标、首页共享同一个 key
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });
  // 卡片上要显示"这是哪个任务的"。在跑的 run 变化没那么快，10 秒够了。
  const running = resource<RunSummary[]>(
    'runs:running',
    (signal) =>
      listRuns({ status: ['running'], limit: 100 }, signal)
        .then((page) => page.items)
        .catch((e) => {
          ignoreForbidden(e);
          return [] as RunSummary[];
        }),
    { pollMs: 10_000 }
  );

  const items = $derived(approvals.data ?? []);
  const loaded = $derived(!approvals.pending);
  const error = $derived(approvals.error ? describeError(approvals.error) : null);

  // 任务列表和 /tasks、命令面板共享同一个 key，所以这里不额外产生请求
  const tasks = resource<TaskSummary[]>(
    'tasks',
    (signal) => listAllTasks(signal),
    { ttlMs: 30_000 }
  );

  /** run_id -> 任务名。原来是每张卡片各做两次 find，现在建一次 Map。 */
  const taskNames = $derived.by(() => {
    const byTask = new Map((tasks.data ?? []).map((t) => [t.id, t.name]));
    return new Map(
      (running.data ?? []).map((r) => [r.id, byTask.get(r.task_id) ?? null])
    );
  });
</script>

<PageHeader title="待审批">
  {#if items.length}<span class="tag warn">{items.length} 个等待中</span>{/if}
  {#snippet sub()}
    <span>超时未决一律按<b>拒绝</b>处理——审批门的意义就在于「没人点头就不做」。</span>
  {/snippet}
</PageHeader>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={3} /></div>
{:else}
  <div class="list">
    {#each items as approval (approval.id)}
      <ApprovalCard {approval} taskName={taskNames.get(approval.run_id) ?? null} ondecided={() => invalidate('approvals', 'overview')} />
    {:else}
      <Empty
        title="没有待审批的操作"
        hint="AI 命中 ask 策略、或者跑到「人工确认」步骤时，卡片会出现在这里，侧栏也会亮起数字。"
      />
    {/each}
  </div>
{/if}

<style>
  .list {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
</style>
