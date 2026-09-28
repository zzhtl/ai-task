<script lang="ts">
  /**
   * 硬策略：在工具调用边界强制拦截，模型绕不过去。
   *
   * 和软规则分成两个组件。它们的字段、语义和危险程度都不一样，
   * 写在一起容易顺手把两张表合并——而「以为写一条规则就管住了」
   * 正是这类系统最常见的致命误解。
   *
   * 表单 ⇄ 报文的转换在 policyForm.ts 里（纯函数、有往返单测）：
   * 编辑一条规则时，表单管不到的东西必须原样带回去，空值就是"不设"。
   */
  import { api } from '$api/client';
  import type { Rule } from '$api/models';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import HelpTip from '$lib/ui/HelpTip.svelte';
  import type { PolicyEvaluation } from '$api/types/PolicyEvaluation';
  import PolicyEvaluate from './PolicyEvaluate.svelte';
  import type { TabShared } from './types';
  import {
    PATTERN_KINDS,
    emptyPolicyForm,
    evaluationOrder,
    patternRows,
    policyBody,
    policyFormFromRule,
    policyProblems,
    type PatternKind,
    type PolicyForm
  } from './policyForm';

  let {
    items,
    loaded,
    busy,
    adding,
    editing,
    fieldErr,
    act,
    onclose,
    onedit,
    ondelete,
    ontoggle,
    onnew
  }: TabShared & {
    items: Rule[];
    loaded: boolean;
    onedit: (rule: Rule) => void;
    ondelete: (rule: Rule) => void;
    ontoggle: (rule: Rule) => void;
    onnew: () => void;
  } = $props();

  /** 常见工具名。只是建议，照样可以填别的（MCP 工具之类）。 */
  const TOOLS = [
    'Bash',
    'Read',
    'Write',
    'Edit',
    'Glob',
    'Grep',
    'WebFetch',
    'WebSearch',
    'remote_bash',
    'remote_read',
    'remote_write',
    'remote_glob',
    'remote_grep'
  ];
  const ARGS = ['command', 'file_path', 'path', 'pattern', 'url'];

  let form = $state<PolicyForm>(emptyPolicyForm());
  /** 主机 tag 在输入框里是逗号分隔的一行，保存时再拆。 */
  let tagsText = $state('');

  // 表单字段跟着 editing 走，而不是靠一个全局 closeForm 把三组字段一起清
  $effect(() => {
    if (editing && editing.kind !== 'prompt') {
      // 从局部变量读，不从刚写进去的 `form` 读：effect 里读自己写的状态会自我触发，
      // 死循环到 effect_update_depth_exceeded，后面关弹层的更新也跟着断掉
      const next = policyFormFromRule(editing);
      form = next;
      tagsText = next.hostTags.join(', ');
    } else if (!adding) {
      form = emptyPolicyForm();
      tagsText = '';
    }
  });

  const problems = $derived(policyProblems(form, editing === null));

  const save = () =>
    act(
      async () => {
        const body = policyBody({ ...form, hostTags: tagsText.split(',') });
        if (editing) {
          await api(`/api/v1/rules/${editing.id}`, { method: 'PUT', body });
        } else {
          await api('/api/v1/rules', { method: 'POST', body: { ...body, name: form.name } });
        }
        onclose();
      },
      editing ? '策略已更新，立刻对新的工具调用生效' : '策略已添加，立刻对新的工具调用生效'
      // act 已经把错误落到了表单和横幅上，这里不必再抛成未处理的 rejection
    ).catch(() => {});

  function scopeOf(rule: Rule): { hostTags: string[]; taskIds: string[] } {
    const scope = (rule.spec.scope ?? {}) as { host_tags?: string[]; task_ids?: string[] };
    return { hostTags: scope.host_tags ?? [], taskIds: scope.task_ids ?? [] };
  }
  function matcherOf(rule: Rule): { tool: string | null; arg: string | null } {
    const m = (rule.spec.match ?? {}) as { tool?: string | null; arg?: string | null };
    return { tool: m.tool ?? null, arg: m.arg ?? null };
  }

  const EFFECT_TAG: Record<string, string> = { deny: 'danger', ask: 'warn', allow: 'ok' };
  const EFFECT_NAME: Record<string, string> = { deny: '拒绝', ask: '转人工', allow: '放行' };
  const KIND_LABEL = Object.fromEntries(PATTERN_KINDS.map((k) => [k.id, k.label])) as Record<PatternKind, string>;

  /** 按真实判决的顺序分组：照着从上往下读，就是一次工具调用被判的过程。 */
  const order = $derived(evaluationOrder(items));

  /** 试算的结果。命中的那条在列表里标出来。 */
  let hit = $state<PolicyEvaluation | null>(null);

  function positionOf(id: string): string | null {
    const at = order.global.findIndex((r) => r.id === id);
    if (at >= 0) return `第 ${at + 1} 条全局规则`;
    if (order.mounted.some((r) => r.id === id)) return '任务挂载的规则';
    return null;
  }
</script>

<p class="lead">
  在工具调用边界强制执行，模型绕不过去。按优先级从高到低判，<strong>先命中的生效</strong>；要写成白名单：
  先默认拒绝某个工具，再用更高优先级放行具体用法。
  <HelpTip>
    实测教训：只配一条 <code>rm -r</code> 的正则，模型会改用 <code>find -delete</code> 绕过去。
    <code>ask</code> 会把那次调用挂起等人点头，超时按拒绝处理。按任务挂载的规则在该任务里优先级再加 1000。
  </HelpTip>
</p>

<datalist id="policy-tools">
  {#each TOOLS as t (t)}<option value={t}></option>{/each}
</datalist>
<datalist id="policy-args">
  {#each ARGS as a (a)}<option value={a}></option>{/each}
</datalist>

<Modal open={adding} title={editing ? `编辑策略 ${editing.name}` : '新增策略'} size="lg" {onclose}>
  <div class="form-grid">
    <Field
      label="名称"
      hint={editing ? '名字不能改：任务是按名字挂规则的' : undefined}
      error={fieldErr.name}
    >
      {#snippet control(p)}
        <input {...p} bind:value={form.name} placeholder="prod-no-write" spellcheck="false" disabled={editing !== null} />
      {/snippet}
    </Field>
    <Field label="判决" error={fieldErr.effect}>
      {#snippet control(p)}
        <select {...p} bind:value={form.effect}>
          <option value="deny">deny — 直接拒绝</option>
          <option value="ask">ask — 挂起等人审批</option>
          <option value="allow">allow — 放行（配合高优先级做白名单）</option>
        </select>
      {/snippet}
    </Field>
    <Field label="工具" hint="留空 = 任意工具" error={fieldErr.tool}>
      {#snippet control(p)}
        <input {...p} bind:value={form.tool} list="policy-tools" placeholder="任意工具" spellcheck="false" />
      {/snippet}
    </Field>
    <Field label="匹配参数" hint="留空 = 匹配整个输入的 JSON 文本" error={fieldErr.arg}>
      {#snippet control(p)}
        <input {...p} bind:value={form.arg} list="policy-args" placeholder="整个输入" spellcheck="false" />
      {/snippet}
    </Field>
    <Field label="优先级" hint="数值大的先判，首个命中生效；按任务挂载时再 +1000" error={fieldErr.priority}>
      {#snippet control(p)}
        <input {...p} type="number" bind:value={form.priority} />
      {/snippet}
    </Field>
    <Field label="作用范围" error={fieldErr.global}>
      {#snippet control(p)}
        <select {...p} bind:value={form.global}>
          <option value={true}>全局 — 所有任务都生效</option>
          <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
        </select>
      {/snippet}
    </Field>

    <div class="wide patterns">
      <span class="label-text">模式<span class="hint">任一条命中即算匹配；一条都没有 = 只要工具对上就命中</span></span>
      {#each form.patterns as pattern, i (i)}
        <div class="pattern">
          <select bind:value={pattern.kind} aria-label="匹配方式">
            {#each PATTERN_KINDS as k (k.id)}<option value={k.id}>{k.label}</option>{/each}
          </select>
          <input
            bind:value={pattern.value}
            class="mono"
            spellcheck="false"
            aria-label="模式"
            placeholder={pattern.kind === 'regex' ? '^\\s*rm\\s+-rf\\s+/' : pattern.kind === 'glob' ? 'journalctl *' : 'rm -rf'}
          />
          <button
            class="btn-ghost btn-sm btn-icon danger"
            title="删掉这条模式"
            aria-label="删掉这条模式"
            onclick={() => (form.patterns = form.patterns.filter((_, at) => at !== i))}
          >
            <Icon name="close" />
          </button>
        </div>
      {/each}
      <div>
        <button class="btn-sm" onclick={() => (form.patterns = [...form.patterns, { kind: 'regex', value: '' }])}>
          <Icon name="plus" /> 添加模式
        </button>
      </div>
      {#if fieldErr.match}<span class="field-error">{fieldErr.match}</span>{/if}
    </div>

    <Field label="主机 tag" hint="逗号分隔；只对带这些 tag 的主机生效，留空 = 不限主机" error={fieldErr.scope} wide>
      {#snippet control(p)}
        <input {...p} bind:value={tagsText} placeholder="prod, db" spellcheck="false" />
      {/snippet}
    </Field>
    {#if form.taskIds.length}
      <p class="wide faint small">
        另外还限定了 {form.taskIds.length} 个任务（通过接口设置），界面上改不了，保存时原样保留。
      </p>
    {/if}
    <Field
      label="原因"
      hint="会作为 tool_result 回给模型，写清为什么比写「不行」有用"
      error={fieldErr.reason}
      wide
    >
      {#snippet control(p)}
        <input {...p} bind:value={form.reason} placeholder="生产机禁止递归删除" />
      {/snippet}
    </Field>
  </div>
  {#snippet footer()}
    {#if problems.length}<span class="faint small problems">{problems[0]}</span>{/if}
    <button class="btn-ghost" onclick={onclose} disabled={busy}>取消</button>
    <button class="btn-primary" onclick={save} disabled={busy || problems.length > 0}>
      {editing ? '保存' : '添加策略'}
    </button>
  {/snippet}
</Modal>

<PolicyEvaluate onresult={(r) => (hit = r)} position={positionOf} />

{#snippet row(rule: Rule, index: number | null)}
  {@const m = matcherOf(rule)}
  {@const scope = scopeOf(rule)}
  {@const effect = String(rule.spec.effect)}
  <tr class:off={!rule.enabled} class:on={editing?.id === rule.id} class:hit={hit?.rule_id === rule.id}>
    <td class="num faint">{index ?? ''}</td>
    <td class="num nowrap">
      {rule.priority}{#if rule.scope === 'task'}<span class="faint small">+1000</span>{/if}
    </td>
    <td><span class="tag {EFFECT_TAG[effect] ?? ''}">{EFFECT_NAME[effect] ?? effect}</span></td>
    <td class="rule">
      <div class="title">
        <span class="name">{rule.name}</span>
        <span class="mono faint small">{m.tool ?? '任意工具'}{m.arg ? `.${m.arg}` : ''}</span>
      </div>
      {#if patternRows(rule.spec).length}
        <div class="chips">
          {#each patternRows(rule.spec) as p, i (i)}
            <span class="chip"><span class="k">{KIND_LABEL[p.kind]}</span><code>{p.value}</code></span>
          {/each}
        </div>
      {:else}
        <div class="faint small">{m.tool ? `${m.tool} 的所有调用` : '所有工具调用'}</div>
      {/if}
      <div class="reason">{rule.spec.reason}</div>
    </td>
    <td>
      <span class="scope">
        <span class="tag" class:accent={rule.scope === 'global'}>{rule.scope === 'global' ? '全局' : '按任务挂载'}</span>
        {#each scope.hostTags as t (t)}<span class="tag" title="只对带这个 tag 的主机生效">{t}</span>{/each}
        {#if scope.taskIds.length}<span class="tag" title="只对这些任务生效（通过接口设置）">限 {scope.taskIds.length} 个任务</span>{/if}
      </span>
    </td>
    <td class="act">
      <div class="row">
        <button class="btn-ghost btn-sm" disabled={busy} onclick={() => ontoggle(rule)}>
          {rule.enabled ? '停用' : '启用'}
        </button>
        <button class="btn-ghost btn-sm btn-icon" title="编辑" aria-label="编辑 {rule.name}" disabled={busy} onclick={() => onedit(rule)}>
          <Icon name="pencil" />
        </button>
        <!-- 只是暂时不生效的话用停用：删除会让历史 run 的规则指纹对不上 -->
        <button
          class="btn-ghost btn-sm btn-icon danger"
          title="删除"
          aria-label="删除 {rule.name}"
          disabled={busy}
          onclick={() => ondelete(rule)}
        >
          <Icon name="trash" />
        </button>
      </div>
    </td>
  </tr>
{/snippet}

{#if !loaded}
  <div class="card"><Loading rows={3} /></div>
{:else if items.length}
  <div class="card flush">
    <table class="policies">
      <thead>
        <tr><th class="num">#</th><th class="num">优先级</th><th>判决</th><th>规则</th><th>范围</th><th class="act"></th></tr>
      </thead>
      {#if order.mounted.length}
        <tbody>
          <tr class="group">
            <th colspan="6">按任务挂载：只在挂上它的任务里生效，优先级 +1000，排在所有全局规则前面</th>
          </tr>
          {#each order.mounted as rule (rule.id)}{@render row(rule, null)}{/each}
        </tbody>
      {/if}
      <tbody>
        <tr class="group"><th colspan="6">全局：从上往下判，先命中的生效</th></tr>
        {#each order.global as rule, i (rule.id)}{@render row(rule, i + 1)}{/each}
        <tr class="fallback" class:hit={hit?.decided_by === 'default'}>
          <td class="num faint">—</td>
          <td colspan="5">
            <b>都没命中时走兜底</b>
            <span class="faint">：只读工具放行；写类工具在带 <code>prod</code> 标签的主机上转人工确认，其余放行</span>
          </td>
        </tr>
      </tbody>
      {#if order.disabled.length}
        <tbody>
          <tr class="group"><th colspan="6">已停用：不参与判决</th></tr>
          {#each order.disabled as rule (rule.id)}{@render row(rule, null)}{/each}
        </tbody>
      {/if}
    </table>
  </div>
{:else if !adding}
  <Empty title="还没有硬策略" hint="现在所有工具调用都走兜底策略：只读放行，写类工具在 prod 主机上转人工确认。至少给生产机加一条 deny。">
    {#snippet action()}
      <button class="btn-primary" onclick={onnew}>新增策略</button>
    {/snippet}
  </Empty>
{/if}

<style>
  .name {
    font-weight: 500;
  }
  .rule {
    min-width: 24ch;
    max-width: 56ch;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 4px;
  }
  .chip {
    display: inline-flex;
    align-items: baseline;
    gap: 4px;
    max-width: 100%;
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-radius: var(--r1);
    background: var(--surface-2);
    font-size: var(--t-xs);
  }
  .chip .k {
    color: var(--fg-faint);
  }
  .chip code {
    overflow-wrap: anywhere;
  }
  .reason {
    margin-top: 4px;
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .scope {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .policies tr.group th {
    padding: var(--s3) var(--s4) var(--s2);
    background: var(--surface-2);
    font-weight: 500;
    color: var(--fg-dim);
    text-align: left;
  }
  .policies tr.hit td {
    background: var(--accent-bg);
  }
  .policies tr.hit td:first-child {
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .fallback td {
    font-size: var(--t-sm);
  }
  .patterns {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    font-size: var(--t-xs);
    color: var(--fg-dim);
  }
  .patterns .hint {
    margin-left: var(--s2);
    color: var(--fg-faint);
  }
  .pattern {
    display: grid;
    grid-template-columns: 8rem minmax(0, 1fr) auto;
    gap: var(--s2);
    align-items: center;
  }
  .problems {
    margin-right: auto;
  }
</style>
