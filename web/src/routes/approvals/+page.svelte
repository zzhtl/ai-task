<script lang="ts">
  import { api } from '$api/client';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';

  interface Approval {
    id: string;
    run_id: string;
    node_key: string | null;
    title: string;
    intent: Record<string, unknown>;
    rule_id: string | null;
    requested_at: string;
    expires_at: string;
    expires_in_s: number;
  }

  let items = $state<Approval[]>([]);
  let error = $state<string | null>(null);

  async function load() {
    try {
      const page = await api<{ items: Approval[] }>('/api/v1/approvals');
      items = page.items;
      error = null;
    } catch (e) {
      error = String(e);
    }
  }

  // 审批是"人正坐在那里等"的场景，但决策可能来自别人。5 秒刷一次够了，
  // 为它开一条 SSE 通道不值得。
  $effect(() => {
    void load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });
</script>

<PageHeader title="待审批">
  {#snippet sub()}
    <span>超时未决一律按<b>拒绝</b>处理——审批门的意义就在于「没人点头就不做」。</span>
  {/snippet}
  {#snippet actions()}
    {#if items.length}<span class="tag danger">{items.length} 个等待中</span>{/if}
  {/snippet}
</PageHeader>

{#if error}<p class="bad">{error}</p>{/if}

<div class="list">
  {#each items as approval (approval.id)}
    <ApprovalCard {approval} ondecided={load} />
  {:else}
    <Empty title="没有待审批的操作" hint="AI 命中 ask 策略、或者跑到 approval 节点时，卡片会出现在这里。" />
  {/each}
</div>

<style>
  .list { display: flex; flex-direction: column; gap: var(--s3); }
</style>
