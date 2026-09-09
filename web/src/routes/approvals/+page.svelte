<script lang="ts">
  import { describeError } from '$api/client';
  import { listApprovals, type Approval } from '$api/models';
  import { listRuns, listTasks } from '$api/runs';
  import type { RunSummary } from '$api/types/RunSummary';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';

  let items = $state<Approval[]>([]);
  let runs = $state<RunSummary[]>([]);
  let tasks = $state<TaskSummary[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);

  async function load() {
    try {
      const [a, r, t] = await Promise.all([
        listApprovals(),
        listRuns({ status: ['running'], limit: 100 }).catch(() => ({ items: [] as RunSummary[] })),
        listTasks().catch(() => ({ items: [] as TaskSummary[] }))
      ]);
      items = a;
      runs = r.items;
      tasks = t.items;
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  // 审批是"人正坐在那里等"的场景，但决策可能来自别人。5 秒刷一次够了，
  // 为它开一条 SSE 通道不值得。
  $effect(() => {
    void load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });

  const taskOf = (runId: string) => {
    const run = runs.find((r) => r.id === runId);
    return run ? (tasks.find((t) => t.id === run.task_id)?.name ?? null) : null;
  };
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
      <ApprovalCard {approval} taskName={taskOf(approval.run_id)} ondecided={load} />
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
