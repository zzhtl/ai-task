<script lang="ts">
  /**
   * 审批卡片。
   *
   * 渲染的是**结构化意图**，不是一段自然语言。要让人在几秒内判断"这该不该做"，
   * 必须看得见具体的东西：哪台机器、跑什么命令、改哪个文件。
   * 一段"我将执行一些维护操作"只会让审批退化成无脑点通过。
   */
  import { session } from '$lib/auth/session.svelte';
  import { api, describeError } from '$api/client';
  import type { Approval } from '$api/models';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { mmss } from '$lib/ui/format';

  let { approval, ondecided }: { approval: Approval; ondecided?: () => void } = $props();

  let reason = $state('');
  const canDecide = $derived(session.can('operator'));
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

  const show = (value: unknown) => (typeof value === 'string' ? value : JSON.stringify(value, null, 2));

  /**
   * 策略要求确认的那次工具调用：intent 里带 `tool` 和 `input`。
   * 最该被看见的是主参数——要跑的命令、要改的文件——放进醒目的代码块，其余参数收起来。
   */
  const call = $derived.by(() => {
    const intent = approval.intent ?? {};
    if (typeof intent.tool !== 'string') return null;
    const input =
      intent.input && typeof intent.input === 'object' && !Array.isArray(intent.input)
        ? (intent.input as Record<string, unknown>)
        : {};
    const key = ['command', 'file_path', 'path', 'url', 'pattern'].find((k) => typeof input[k] === 'string') ?? null;
    return {
      tool: intent.tool.replace(/^mcp__ai_task_remote__/, ''),
      key,
      main: key ? String(input[key]) : null,
      rest: Object.entries(input).filter(([k]) => k !== key)
    };
  });
  const policyReason = $derived(
    typeof approval.intent?.policy_reason === 'string' ? approval.intent.policy_reason : null
  );
  /** 不是工具调用的审批（审批步骤）：把意图里其余的东西原样列出来。节点已经在标题下面了。 */
  const otherRows = $derived(
    call
      ? []
      : Object.entries(approval.intent ?? {})
          .filter(([k]) => k !== 'node')
          .map(([key, value]) => ({ key, text: show(value) }))
  );

  /** 整个审批窗口有多长。两个时刻都是服务端给的，不受本地时钟影响。 */
  const total = $derived(
    Math.max(1, (Date.parse(approval.expires_at) - Date.parse(approval.requested_at)) / 1000)
  );
</script>

<article class:expiring={remaining > 0 && remaining < 60} class:expired={remaining === 0}>
  <header>
    <div class="head-main">
      <h3>{approval.title}</h3>
      <p class="meta">
        <a href="/tasks/{approval.task_id}">{approval.task_name}</a>
        {#if approval.node_key}<span>步骤 <code>{approval.node_key}</code></span>{/if}
        {#if call}<span>{approval.host_name ? `目标机 ${approval.host_name}` : '中心节点本机'}</span>{/if}
        <span class="faint">{approval.rule_id ? '策略要求确认' : '审批步骤'}</span>
        <a class="faint" href="/runs/{approval.run_id}">看这次执行</a>
      </p>
    </div>
    <span class="clock" title="超时后自动拒绝">
      {remaining === 0 ? '已超时（自动拒绝）' : `剩余 ${mmss(remaining)}`}
    </span>
  </header>
  <div
    class="countdown"
    role="progressbar"
    aria-label="审批剩余时间"
    aria-valuemin={0}
    aria-valuemax={Math.round(total)}
    aria-valuenow={remaining}
  >
    <span style="width: {Math.min(100, (remaining / total) * 100)}%"></span>
  </div>

  {#if call}
    <div class="call">
      <div class="call-head">
        <span class="tool mono">{call.tool}</span>
        {#if call.key}<span class="faint small">{call.key}</span>{/if}
      </div>
      {#if call.main !== null}<pre class="main">{call.main}</pre>{/if}
      {#if call.rest.length}
        <details>
          <summary class="small">其余参数（{call.rest.length}）</summary>
          <dl class="intent">
            {#each call.rest as [key, value] (key)}
              <dt>{key}</dt>
              <dd><code>{show(value)}</code></dd>
            {/each}
          </dl>
        </details>
      {/if}
    </div>
    {#if policyReason}<p class="why"><span class="faint">为什么要确认：</span>{policyReason}</p>{/if}
  {:else if otherRows.length}
    <dl class="intent">
      {#each otherRows as row (row.key)}
        <dt>{row.key}</dt>
        <dd><code>{row.text}</code></dd>
      {/each}
    </dl>
  {/if}

  {#if error}<div class="banner">{error}</div>{/if}

  <div class="actions">
    <input
      bind:value={reason}
      placeholder={canDecide ? '理由（可留空；会作为 tool_result 回给模型）' : '需要 operator 权限才能审批'}
      disabled={busy || remaining === 0 || !canDecide}
    />
    <button onclick={() => decide(false)} disabled={busy || remaining === 0 || !canDecide} class="btn-ghost danger">
      拒绝
    </button>
    <button onclick={() => decide(true)} disabled={busy || remaining === 0 || !canDecide} class="btn-primary">批准</button>
  </div>
</article>

<style>
  article {
    border: 1px solid var(--warn-border);
    border-left: 3px solid var(--warn-fg);
    border-radius: var(--r3);
    padding: var(--s4);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  /* 最后一分钟变红。审批卡是会过期的，而过期等于拒绝 */
  article.expiring {
    border-color: var(--bad-border);
    border-left-color: var(--bad-fg);
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
    display: flex;
    flex-wrap: wrap;
    gap: 2px var(--s3);
    margin: 4px 0 0;
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .countdown {
    height: 3px;
    border-radius: 2px;
    background: var(--surface-2);
    overflow: hidden;
  }
  .countdown span {
    display: block;
    height: 100%;
    background: var(--warn-fg);
    transition: width 1s linear;
  }
  article.expiring .countdown span {
    background: var(--bad-fg);
  }
  .call {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .call-head {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
  }
  .tool {
    font-weight: 600;
  }
  .main {
    margin: 0;
    padding: var(--s3);
    border: 1px solid var(--line-strong);
    border-radius: var(--r2);
    background: var(--surface-2);
    font-size: var(--t-base);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 14rem;
    overflow: auto;
  }
  .why {
    margin: 0;
    font-size: var(--t-sm);
  }
  details summary {
    cursor: pointer;
    color: var(--fg-dim);
  }
  details .intent {
    margin-top: var(--s2);
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
    display: block;
    font-size: var(--t-sm);
    color: var(--fg);
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
