<script lang="ts">
  /**
   * 试算：拿一次假想的工具调用问"会怎样"。
   *
   * 边填边算：停手 300ms 发一次，旧请求作废。服务端和真实调用走同一个判决函数，
   * 所以这里说放行，真跑时就是放行——这是这个面板唯一的价值，不能有第二套口径。
   */
  import { describeError } from '$api/client';
  import { evaluatePolicy } from '$api/models';
  import { listAllTasks } from '$api/runs';
  import type { PolicyEffect } from '$api/types/PolicyEffect';
  import type { PolicyEvaluation } from '$api/types/PolicyEvaluation';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import { argFor, buildInput, type InputMode } from './evaluate';
  import { normalizeTags } from './policyForm';

  let {
    onresult,
    position
  }: {
    /** 结果变了（含清空）。页面据此在列表里标出命中的那条。 */
    onresult: (result: PolicyEvaluation | null) => void;
    /** 命中的规则在判决顺序里的位置说明，比如"第 2 条全局规则"。 */
    position: (ruleId: string) => string | null;
  } = $props();

  const EFFECT_NAME: Record<PolicyEffect, string> = { allow: '放行', deny: '拒绝', ask: '转人工确认' };
  const EFFECT_TAG: Record<PolicyEffect, string> = { allow: 'ok', deny: 'danger', ask: 'warn' };

  // 默认填一个最常见的问题，打开就能看到一个结论
  let tool = $state('Bash');
  let mode = $state<InputMode>('field');
  let value = $state('rm -rf /var/lib/mysql');
  let raw = $state('');
  let tagsText = $state('prod');
  let taskId = $state('');
  let dryRun = $state(false);

  let tasks = $state<TaskSummary[]>([]);
  $effect(() => {
    listAllTasks()
      .then((t) => (tasks = t))
      .catch(() => {});
  });

  const arg = $derived(argFor(tool));
  // 认不出的工具没有"主参数"可填，只能写原始 JSON
  $effect(() => {
    if (!arg && mode === 'field') mode = 'json';
  });

  const built = $derived(buildInput(tool, mode, value, raw));
  let result = $state<PolicyEvaluation | null>(null);
  let error = $state<string | null>(null);
  let pending = $state(false);

  $effect(() => {
    const request = built;
    const name = tool.trim();
    const tags = normalizeTags(tagsText.split(','));
    const task = taskId || null;
    const shadow = dryRun;
    if ('error' in request || !name) {
      result = null;
      error = 'error' in request ? request.error : null;
      pending = false;
      onresult(null);
      return;
    }
    const controller = new AbortController();
    pending = true;
    const timer = setTimeout(async () => {
      try {
        const r = await evaluatePolicy(
          { tool: name, input: request.input, host_tags: tags, task_id: task, dry_run: shadow },
          controller.signal
        );
        result = r;
        error = null;
        onresult(r);
      } catch (e) {
        if (controller.signal.aborted) return;
        result = null;
        error = describeError(e);
        onresult(null);
      } finally {
        if (!controller.signal.aborted) pending = false;
      }
    }, 300);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  });

  function source(r: PolicyEvaluation): string {
    if (r.decided_by === 'rule') {
      const at = r.rule_id ? position(r.rule_id) : null;
      return `命中${at ? `${at}` : ''}「${r.rule_name ?? r.rule_id}」`;
    }
    if (r.decided_by === 'default') return '一条规则都没命中，走兜底策略';
    if (r.decided_by === 'invalid_rules') return '有规则编译不了：真实调用时整套策略装不起来，所有工具调用都会被拒';
    return r.decided_by;
  }
</script>

<section class="card evaluate">
  <header class="card-head">
    <h2>试算</h2>
    <span class="sub">拿一次假想的工具调用看会怎样，不会真的执行，也不记审计</span>
  </header>
  <div class="inputs">
    <label class="field">
      <span class="label-text">工具</span>
      <input bind:value={tool} list="policy-tools" class="mono" spellcheck="false" />
    </label>
    <label class="field grow">
      <span class="label-text">
        {mode === 'field' ? `参数 ${arg}` : '参数（原始 JSON）'}
        {#if arg}
          <button class="link-btn" type="button" onclick={() => (mode = mode === 'field' ? 'json' : 'field')}>
            {mode === 'field' ? '写原始 JSON' : `只填 ${arg}`}
          </button>
        {/if}
      </span>
      {#if mode === 'field'}
        <input bind:value class="mono" spellcheck="false" placeholder="留空 = 这个工具的任意调用" />
      {:else}
        <textarea bind:value={raw} class="mono" rows="2" spellcheck="false" placeholder={'{"command": "ls -la"}'}></textarea>
      {/if}
    </label>
    <label class="field">
      <span class="label-text">主机 tag</span>
      <input bind:value={tagsText} placeholder="逗号分隔，留空 = 本机" spellcheck="false" />
    </label>
    <label class="field">
      <span class="label-text">任务</span>
      <select bind:value={taskId}>
        <option value="">不指定（只判全局规则）</option>
        {#each tasks as t (t.id)}<option value={t.id}>{t.name}</option>{/each}
      </select>
    </label>
    <label class="check">
      <input type="checkbox" bind:checked={dryRun} />
      按影子执行判
    </label>
  </div>

  <div class="result" aria-live="polite" class:stale={pending}>
    {#if error}
      <p class="field-error">{error}</p>
    {:else if result}
      <div class="verdict">
        <span class="tag {EFFECT_TAG[result.effect]}">{EFFECT_NAME[result.effect]}</span>
        <span>{source(result)}</span>
      </div>
      {#if result.effect !== result.policy_effect}
        <p class="small">
          策略本身是「{EFFECT_NAME[result.policy_effect]}」；影子执行里写类工具只记录意图、不执行。
        </p>
      {/if}
      <p class="reason"><span class="faint">回给模型：</span>{result.reason}</p>
      <p class="faint small">
        按 <code>{result.tool}</code> 匹配 · 判了 {result.considered} 条规则
      </p>
    {:else if pending}
      <p class="faint small">算着…</p>
    {/if}
  </div>
</section>

<style>
  .evaluate {
    margin-bottom: var(--s4);
  }
  .inputs {
    display: grid;
    grid-template-columns: minmax(8rem, 12rem) minmax(0, 1fr) minmax(8rem, 12rem) minmax(10rem, 14rem) auto;
    gap: var(--s3);
    align-items: end;
  }
  @media (max-width: 960px) {
    .inputs {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
    .grow {
      grid-column: 1 / -1;
    }
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    min-width: 0;
  }
  .label-text {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    font-size: var(--t-xs);
    color: var(--fg-dim);
  }
  .link-btn {
    height: auto;
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent-fg);
    font-size: var(--t-xs);
    cursor: pointer;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: var(--s2);
    height: 34px;
    font-size: var(--t-sm);
    white-space: nowrap;
  }
  .result {
    margin-top: var(--s3);
    padding-top: var(--s3);
    border-top: 1px solid var(--line);
    min-height: 3.5rem;
    transition: opacity var(--dur-1) var(--ease);
  }
  .result.stale {
    opacity: 0.6;
  }
  .result p {
    margin: var(--s1) 0 0;
  }
  .verdict {
    display: flex;
    align-items: center;
    gap: var(--s2);
    font-weight: 500;
  }
  .reason {
    font-size: var(--t-sm);
  }
</style>
