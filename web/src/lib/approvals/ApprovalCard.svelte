<script lang="ts">
  /**
   * 审批卡片。
   *
   * 渲染的是**结构化意图**，不是一段自然语言。要让人在几秒内判断"这该不该做"，
   * 必须看得见具体的东西：哪台机器、跑什么命令、改哪个文件。
   * 一段"我将执行一些维护操作"只会让审批退化成无脑点通过。
   */
  import { api } from '$api/client';

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

  let { approval, ondecided }: { approval: Approval; ondecided?: () => void } = $props();

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
        // body 传对象：api() 自己会 stringify，这里再 stringify 一次
        // 发出去的就是一个 JSON 字符串而不是对象，服务端 422
        { method: 'POST', body: { approved, reason: reason || null } }
      );
      // 并发决策：两个人同时看到卡片是常态。生效的不是你那次时要说出来，
      // 否则点了"拒绝"的人会以为自己拦住了。
      if (!result.was_first) {
        error = `已经有人先决策过了，实际生效的是「${result.approved ? '批准' : '拒绝'}」`;
      }
      ondecided?.();
    } catch (e) {
      error = String(e);
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

  function mmss(total: number): string {
    const m = Math.floor(total / 60);
    return `${m}:${String(total % 60).padStart(2, '0')}`;
  }
</script>

<article class:expiring={remaining > 0 && remaining < 60} class:expired={remaining === 0}>
  <header>
    <h3>{approval.title}</h3>
    <span class="clock" title="超时后自动拒绝">
      {remaining === 0 ? '已超时（自动拒绝）' : `剩余 ${mmss(remaining)}`}
    </span>
  </header>

  <p class="meta muted">
    <a href="/runs/{approval.run_id}">run {approval.run_id.slice(0, 8)}</a>
    {#if approval.node_key}<span class="mono">· {approval.node_key}</span>{/if}
    {#if approval.rule_id}<span>· 由策略触发</span>{/if}
  </p>

  <dl class="intent">
    {#each rows as row (row.key)}
      <dt>{row.key}</dt>
      <dd class:block={row.long}><code>{row.text}</code></dd>
    {/each}
  </dl>

  {#if error}<p class="bad">{error}</p>{/if}

  <div class="actions">
    <input
      bind:value={reason}
      placeholder="理由（会作为 tool_result 回给模型，写清为什么比写不行有用）"
      disabled={busy || remaining === 0}
    />
    <button onclick={() => decide(false)} disabled={busy || remaining === 0} class="deny">
      拒绝
    </button>
    <button onclick={() => decide(true)} disabled={busy || remaining === 0} class="approve">
      批准
    </button>
  </div>
</article>

<style>
  article {
    border: 1px solid var(--line);
    border-left: 3px solid var(--muted);
    border-radius: 0.75rem;
    padding: 0.8rem 1rem;
    background: var(--card);
  }
  article.expiring {
    border-left-color: var(--bad);
  }
  article.expired {
    opacity: 0.55;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
  }
  h3 {
    margin: 0;
    font-size: 1rem;
  }
  .clock {
    font-variant-numeric: tabular-nums;
    font-size: 0.85rem;
    color: var(--muted);
  }
  article.expiring .clock {
    color: var(--bad);
  }
  .meta {
    margin: 0.2rem 0 0.6rem;
    font-size: 0.82rem;
  }
  .intent {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0 0 0.7rem;
    font-size: 0.85rem;
  }
  dt {
    color: var(--muted);
    white-space: nowrap;
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  dd.block code {
    display: block;
    white-space: pre-wrap;
    max-height: 12rem;
    overflow: auto;
  }
  code {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  input {
    flex: 1;
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--line);
    border-radius: 0.4rem;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
  }
  button {
    padding: 0.35rem 1rem;
    border: 1px solid var(--line);
    border-radius: 0.4rem;
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  button:disabled {
    cursor: default;
    opacity: 0.5;
  }
  .approve {
    border-color: var(--ok);
    color: var(--ok);
  }
  .deny {
    border-color: var(--bad);
    color: var(--bad);
  }
  .bad {
    color: var(--bad);
  }
  .muted {
    color: var(--muted);
  }
  .mono {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
</style>
