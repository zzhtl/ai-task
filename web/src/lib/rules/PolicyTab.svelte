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
  import type { TabShared } from './types';
  import {
    PATTERN_KINDS,
    describeMatcher,
    emptyPolicyForm,
    policyBody,
    policyFormFromRule,
    policyProblems,
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

  function hostTags(rule: Rule): string[] {
    const scope = (rule.spec.scope ?? {}) as { host_tags?: string[] };
    return scope.host_tags ?? [];
  }

  const EFFECT_TAG: Record<string, string> = { deny: 'danger', ask: 'warn', allow: 'ok' };
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

{#if !loaded}
  <div class="card"><Loading rows={3} /></div>
{:else if items.length}
  <div class="card flush">
    <table>
      <thead>
        <tr><th>名称</th><th>判决</th><th>匹配</th><th>原因</th><th>范围</th><th>优先级</th><th class="act"></th></tr>
      </thead>
      <tbody>
        {#each items as rule (rule.id)}
          <tr class:off={!rule.enabled} class:on={editing?.id === rule.id}>
            <td class="mono name">{rule.name}</td>
            <td><span class="tag {EFFECT_TAG[String(rule.spec.effect)] ?? ''}">{rule.spec.effect}</span></td>
            <td class="mono small">{describeMatcher(rule.spec)}</td>
            <td class="muted reason">{rule.spec.reason}</td>
            <td>
              <span class="scope">
                <span class="tag" class:accent={rule.scope === 'global'}>
                  {rule.scope === 'global' ? '全局' : '按任务挂载'}
                </span>
                {#each hostTags(rule) as t (t)}<span class="tag" title="只对带这个 tag 的主机生效">{t}</span>{/each}
              </span>
            </td>
            <td class="faint">{rule.priority}</td>
            <td class="act">
              <div class="row">
                <button class="btn-ghost btn-sm" disabled={busy} onclick={() => ontoggle(rule)}>
                  {rule.enabled ? '停用' : '启用'}
                </button>
                <button class="btn-ghost btn-sm btn-icon" title="编辑" aria-label="编辑" disabled={busy} onclick={() => onedit(rule)}>
                  <Icon name="pencil" />
                </button>
                <!-- 只是暂时不生效的话用停用：删除会让历史 run 的规则指纹对不上 -->
                <button
                  class="btn-ghost btn-sm btn-icon danger"
                  title="删除"
                  aria-label="删除规则"
                  disabled={busy}
                  onclick={() => ondelete(rule)}
                >
                  <Icon name="trash" />
                </button>
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if !adding}
  <Empty title="还没有硬策略" hint="现在模型的每一次工具调用都只受执行器默认白名单约束。至少给生产机加一条 deny。">
    {#snippet action()}
      <button class="btn-primary" onclick={onnew}>新增策略</button>
    {/snippet}
  </Empty>
{/if}

<style>
  .name {
    font-weight: 500;
  }
  .reason {
    max-width: 36ch;
  }
  .scope {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
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
