<script lang="ts">
  /**
   * 硬策略：在工具调用边界强制拦截，模型绕不过去。
   *
   * 和软规则分成两个组件。它们的字段、语义和危险程度都不一样，
   * 写在一起容易顺手把两张表合并——而「以为写一条规则就管住了」
   * 正是这类系统最常见的致命误解。
   */
  import { api } from '$api/client';
  import type { Rule } from '$api/models';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import type { TabShared } from './types';

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

  interface Matcher {
    tool?: string;
    arg?: string;
    any_of?: Array<Record<string, string>>;
  }

  let name = $state('');
  let tool = $state('Bash');
  let arg = $state('command');
  let pattern = $state('');
  let kind = $state<'regex' | 'glob' | 'contains'>('regex');
  let verdict = $state<'deny' | 'ask' | 'allow'>('deny');
  let reason = $state('');
  let priority = $state(100);
  let global = $state(true);

  function reset() {
    name = '';
    tool = 'Bash';
    arg = 'command';
    pattern = '';
    kind = 'regex';
    verdict = 'deny';
    reason = '';
    priority = 100;
    global = true;
  }

  // 表单字段跟着 editing 走，而不是靠一个全局 closeForm 把三组字段一起清
  $effect(() => {
    if (editing && editing.kind !== 'prompt') {
      const m = (editing.spec.match ?? {}) as Matcher;
      const first = m.any_of?.[0];
      const [k, v] = first ? (Object.entries(first)[0] ?? ['regex', '']) : ['regex', ''];
      name = editing.name;
      tool = m.tool ?? '';
      arg = m.arg ?? 'command';
      kind = (['regex', 'glob', 'contains'].includes(k) ? k : 'regex') as typeof kind;
      pattern = v;
      verdict = String(editing.spec.effect ?? 'deny') as typeof verdict;
      reason = String(editing.spec.reason ?? '');
      priority = editing.priority;
      global = editing.scope === 'global';
    } else if (!adding) {
      reset();
    }
  });

  const payload = () => ({
    kind: 'policy',
    global,
    priority,
    effect: verdict,
    reason,
    match: pattern ? { tool, arg, any_of: [{ [kind]: pattern }] } : { tool }
  });

  const save = () =>
    act(
      async () => {
        if (editing) {
          await api(`/api/v1/rules/${editing.id}`, { method: 'PUT', body: payload() });
        } else {
          await api('/api/v1/rules', { method: 'POST', body: { ...payload(), name } });
        }
        onclose();
      },
      editing ? '策略已更新，立刻对新的工具调用生效' : '策略已添加，立刻对新的工具调用生效'
    );

  /** 匹配条件压成一行给人扫。 */
  function matcherText(rule: Rule): string {
    const m = (rule.spec.match ?? {}) as Matcher;
    const pats = (m.any_of ?? []).map((p) => {
      const [k, v] = Object.entries(p)[0] ?? ['', ''];
      return `${k} ${v}`;
    });
    const head = m.tool ?? '任意工具';
    if (!pats.length) return `${head} 的所有调用`;
    return `${head}${m.arg ? `.${m.arg}` : ''} ~ ${pats.join(' | ')}`;
  }

  const EFFECT_TAG: Record<string, string> = { deny: 'danger', ask: 'warn', allow: 'ok' };
</script>

<div class="callout">
  <strong>硬策略在工具调用边界强制执行，模型绕不过去。</strong>
  实测教训：只配一条 <code>rm -r</code> 的正则，模型会改用 <code>find -delete</code> 绕过去。
  <strong>要写成白名单形状</strong>——默认拒绝某个工具，再用更高优先级的规则放行具体的用法。
  <code>ask</code> 会把那次工具调用挂起等人点头，超时按拒绝处理。
</div>

<Modal open={adding} title={editing ? `编辑策略 ${editing.name}` : '新增策略'} size="lg" {onclose}>
  <div class="form-grid">
    <Field
      label="名称"
      hint={editing ? '名字不能改：任务是按名字挂规则的' : undefined}
      error={fieldErr.name}
    >
      {#snippet control(p)}
        <input {...p} bind:value={name} placeholder="prod-no-write" spellcheck="false" disabled={editing !== null} />
      {/snippet}
    </Field>
    <Field label="工具" error={fieldErr.match}>
      {#snippet control(p)}
        <input {...p} bind:value={tool} placeholder="Bash / remote_bash / remote_write" spellcheck="false" />
      {/snippet}
    </Field>
    <Field label="判决" error={fieldErr.effect}>
      {#snippet control(p)}
        <select {...p} bind:value={verdict}>
          <option value="deny">deny — 直接拒绝</option>
          <option value="ask">ask — 挂起等人审批</option>
          <option value="allow">allow — 放行（配合高优先级做白名单）</option>
        </select>
      {/snippet}
    </Field>
    <Field label="优先级" hint="数值大的先判，首个命中生效" error={fieldErr.priority}>
      {#snippet control(p)}
        <input {...p} type="number" bind:value={priority} />
      {/snippet}
    </Field>
    <Field label="作用范围" error={fieldErr.global}>
      {#snippet control(p)}
        <select {...p} bind:value={global}>
          <option value={true}>全局 — 所有任务都生效</option>
          <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
        </select>
      {/snippet}
    </Field>
    <Field label="匹配参数" error={fieldErr.arg}>
      {#snippet control(p)}
        <input {...p} bind:value={arg} placeholder="command / path" spellcheck="false" />
      {/snippet}
    </Field>
    <Field label="匹配方式" error={fieldErr.kind}>
      {#snippet control(p)}
        <select {...p} bind:value={kind}>
          <option value="regex">正则</option>
          <option value="glob">glob</option>
          <option value="contains">包含</option>
        </select>
      {/snippet}
    </Field>
    <Field label="模式" hint="留空表示匹配这个工具的所有调用" error={fieldErr.pattern} wide>
      {#snippet control(p)}
        <input {...p} bind:value={pattern} placeholder={'^\\s*rm\\s+-rf\\s+/'} spellcheck="false" class="mono" />
      {/snippet}
    </Field>
    <Field
      label="原因"
      hint="会作为 tool_result 回给模型，写清为什么比写「不行」有用"
      error={fieldErr.reason}
      wide
    >
      {#snippet control(p)}
        <input {...p} bind:value={reason} placeholder="生产机禁止递归删除" />
      {/snippet}
    </Field>
  </div>
  {#snippet footer()}
    <button class="btn-ghost" onclick={onclose} disabled={busy}>取消</button>
    <button class="btn-primary" onclick={save} disabled={busy || !name || !reason}>
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
            <td class="mono small">{matcherText(rule)}</td>
            <td class="muted reason">{rule.spec.reason}</td>
            <td>
              <span class="tag" class:accent={rule.scope === 'global'}>
                {rule.scope === 'global' ? '全局' : '按任务挂载'}
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
</style>
