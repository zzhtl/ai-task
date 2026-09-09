<script lang="ts">
  import { api } from '$api/client';
  import ApprovalCard from '$lib/approvals/ApprovalCard.svelte';

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

<header>
  <a href="/" class="muted">← 返回</a>
  <h1>待审批 {#if items.length}<span class="count">{items.length}</span>{/if}</h1>
</header>

{#if error}<p class="bad">{error}</p>{/if}

<div class="list">
  {#each items as approval (approval.id)}
    <ApprovalCard {approval} ondecided={load} />
  {:else}
    <p class="muted">没有待审批的操作。</p>
  {/each}
</div>

<style>
  h1 { margin: 0.5rem 0 1rem; font-size: 1.4rem; }
  .count {
    display: inline-block;
    min-width: 1.4rem;
    padding: 0 0.4rem;
    border-radius: 999px;
    background: var(--bad);
    color: #fff;
    font-size: 0.85rem;
    text-align: center;
  }
  .list { display: flex; flex-direction: column; gap: 0.75rem; }
  .muted { color: var(--muted); }
  .bad { color: var(--bad); }
</style>
