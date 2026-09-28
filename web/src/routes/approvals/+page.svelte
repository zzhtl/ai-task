<script lang="ts">
  /**
   * 待审批。
   *
   * 审批是"人正坐在那里等"的场景，但决策可能来自别人。5 秒刷一次够了，
   * 为它开一条 SSE 通道不值得。
   */
  import { describeError } from '$api/client';
  import { resource, invalidate } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';

  // 和侧栏徽标、首页共享同一个 key。卡片要的任务名、主机名接口里都带着
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });

  const items = $derived(approvals.data ?? []);
  const loaded = $derived(!approvals.pending);
  const error = $derived(approvals.error ? describeError(approvals.error) : null);
</script>

<PageHeader title="待审批" help="超时未决一律按拒绝处理：审批门的意义就在于「没人点头就不做」。">
  {#if items.length}<span class="tag warn">{items.length} 个等待中</span>{/if}
</PageHeader>

{#if error}<div class="banner">{error}</div>{/if}

{#if !loaded}
  <div class="card"><Loading rows={3} /></div>
{:else}
  <div class="list">
    {#each items as approval (approval.id)}
      <ApprovalCard {approval} ondecided={() => invalidate('approvals', 'overview')} />
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
