<script lang="ts">
  /**
   * 审批卡片。
   *
   * 渲染的是**结构化意图**，不是一段自然语言。要让人在几秒内判断"这该不该做"，
   * 必须看得见具体的东西：哪台机器、跑什么命令、改哪个文件。
   * 一段"我将执行一些维护操作"只会让审批退化成无脑点通过。
   */
  import { api, describeError } from '$api/client';
  import type { Approval } from '$api/models';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { mmss } from '$lib/ui/format';

  let {
    approval,
    taskName = null,
    ondecided
  }: { approval: Approval; taskName?: string | null; ondecided?: () => void } = $props();

  let reason = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  /**
   * 剩余秒数以**服务端给的**为基准，本地只往前推秒数。
   * 自己按 expires_at 减本地时钟的话，客户端时钟偏几分钟就会把还能点的卡片
   * 显示成已超时（或者反过来，点下去才发现早过期了）。
   */
  let ticked = $state(0);
  const remaining = $derived(Math.max(0, approval.expires_in_s - ticked));

  $effect(() => {
    // 换了一张卡片就从头数
    void approval.id;
    ticked = 0;
    const timer = setInterval(() => (ticked += 1), 1000);
    return () => clearInterval(timer);
  });

  async function decide(approved: boolean) {
    busy = true;
    error = null;
    try {
      const result = await api<{ approved: boolean; was_first: boolean }>(
        `/api/v1/approvals/${approval.id}/decide`,
        { method: 'POST', body: { approved, reason: reason || null } }
      );
      // 并发决策：两个人同时看到卡片是常态。生效的不是你那次时要说出来，
      // 否则点了"拒绝"的人会以为自己拦住了。
      if (!result.was_first) {
        toastError(`已经有人先决策过了，实际生效的是「${result.approved ? '批准' : '拒绝'}」`);
      } else {
        toast(approved ? '已批准，执行继续' : '已拒绝');
      }
      ondecided?.();
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }

  /** 意图里最该被看见的几项，按重要性排。 */
  const HIGHLIGHT = ['tool', 'host', 'host_id', 'command', 'path', 'policy_reason'] as const;

  const rows = $derived.by(() => {
    const entries = Object.entries(approval.intent ?? {});
    const rank = (k: string) => {
      const i = HIGHLIGHT.indexOf(k as (typeof HIGHLIGHT)[number]);
      return i === -1 ? HIGHLIGHT.length : i;
    };
    return entries
      .sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
      .map(([key, value]) => ({
        key,
        text: typeof value === 'string' ? value : JSON.stringify(value, null, 2),
        long: typeof value !== 'string' || value.length > 60
      }));
  });
</script>

<article class:expiring={remaining > 0 && remaining < 60} class:expired={remaining === 0}>
  <header>
    <div class="head-main">
      <h3>{approval.title}</h3>
      <p class="meta faint">
        {#if taskName}<span>{taskName}</span> ·{/if}
        <a href="/runs/{approval.run_id}">run {approval.run_id.slice(0, 8)}</a>
        {#if approval.node_key}<span class="mono">· {approval.node_key}</span>{/if}
        {#if approval.rule_id}<span>· 由策略 ask 触发</span>{:else}<span>· 审批节点</span>{/if}
      </p>
    </div>
    <span class="clock" title="超时后自动拒绝">
      {remaining === 0 ? '已超时（自动拒绝）' : `剩余 ${mmss(remaining)}`}
    </span>
  </header>

  {#if rows.length}
    <dl class="intent">
      {#each rows as row (row.key)}
        <dt>{row.key}</dt>
        <dd class:block={row.long}><code>{row.text}</code></dd>
      {/each}
    </dl>
  {:else}
    <p class="faint small">这次审批没有附带结构化意图。</p>
  {/if}

  {#if error}<div class="banner">{error}</div>{/if}

  <div class="actions">
    <input
      bind:value={reason}
      placeholder="理由（可留空；会作为 tool_result 回给模型）"
      disabled={busy || remaining === 0}
    />
    <button onclick={() => decide(false)} disabled={busy || remaining === 0} class="btn-danger">拒绝</button>
    <button onclick={() => decide(true)} disabled={busy || remaining === 0} class="approve">批准</button>
  </div>
</article>

<style>
  article {
    border: 1px solid color-mix(in srgb, var(--warn) 30%, var(--line));
    border-left: 3px solid var(--warn);
    border-radius: var(--r3);
    padding: var(--s4);
    background: color-mix(in srgb, var(--warn) 4%, var(--surface-1));
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  /* 最后一分钟变红。审批卡是会过期的，而过期等于拒绝 */
  article.expiring {
    border-color: color-mix(in srgb, var(--bad) 45%, var(--line));
    border-left-color: var(--bad);
    background: color-mix(in srgb, var(--bad) 5%, var(--surface-1));
  }
  article.expired {
    opacity: 0.55;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--s4);
  }
  .head-main {
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: var(--t-lg);
  }
  .meta {
    margin: 2px 0 0;
    font-size: var(--t-sm);
    display: flex;
    gap: var(--s1);
    flex-wrap: wrap;
  }
  .clock {
    font-size: var(--t-base);
    color: var(--warn);
    white-space: nowrap;
    font-weight: 500;
  }
  article.expiring .clock {
    color: var(--bad);
  }
  .intent {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.3rem var(--s3);
    margin: 0;
    font-size: var(--t-base);
    padding: var(--s3);
    background: var(--surface-2);
    border-radius: var(--r2);
  }
  dt {
    color: var(--fg-faint);
    white-space: nowrap;
    font-size: var(--t-sm);
    padding-top: 2px;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
    min-width: 0;
  }
  dd code {
    font-size: var(--t-sm);
    color: var(--fg);
  }
  dd.block code {
    display: block;
    white-space: pre-wrap;
    max-height: 12rem;
    overflow: auto;
  }
  .actions {
    display: flex;
    gap: var(--s2);
  }
  .actions input {
    flex: 1;
  }
  .approve {
    border-color: color-mix(in srgb, var(--ok) 50%, var(--line-strong));
    color: var(--ok);
  }
  .approve:hover:not(:disabled) {
    background: color-mix(in srgb, var(--ok) 12%, var(--surface-2));
    color: var(--ok);
  }
  .banner {
    margin: 0;
  }
  @media (max-width: 640px) {
    .actions {
      flex-wrap: wrap;
    }
    .actions input {
      flex-basis: 100%;
    }
  }
</style>
