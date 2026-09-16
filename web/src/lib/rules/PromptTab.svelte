<script lang="ts">
  /**
   * 软规则：注入 system prompt，只影响模型的倾向。
   *
   * 和硬策略分开成两个组件，不只是为了文件短——它们的字段、语义和危险程度
   * 都不一样，放在一起写容易顺手把两边的表格合并，而那正是这类系统
   * 最常见的致命误解。
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

  let name = $state('');
  let text = $state('');
  let global = $state(false);

  const bodyText = (rule: Rule) => (rule.spec.text as string) ?? '';

  // 表单字段跟着 editing 走。以前是一个 closeForm 负责把三组字段一起清干净，
  // 加一个字段忘了清不会报错，只会在下次打开表单时留着上一次的残值。
  $effect(() => {
    if (editing && editing.kind === 'prompt') {
      name = editing.name;
      text = bodyText(editing);
      global = editing.scope === 'global';
    } else if (!adding) {
      name = '';
      text = '';
      global = false;
    }
  });

  /** 编辑时不带 name：任务按名字挂规则，改名等于把它从所有任务上摘掉。 */
  const payload = () => ({
    kind: 'prompt',
    text,
    global,
    priority: editing?.priority ?? 0
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
      editing ? '软规则已更新，下一次执行开始生效' : '软规则已添加'
    );
</script>

<div class="callout">
  <strong>软规则注入 system prompt，只影响模型的倾向。</strong>
  模型<strong>可以不听</strong>。工具结果是不可信输入，能影响模型对 prompt 的遵守——所以
  真正要拦住的事情必须同时配一条硬策略。软规则的价值在于让模型少走弯路，不在于阻止它。
</div>

<Modal open={adding} title={editing ? `编辑软规则 ${editing.name}` : '新增软规则'} {onclose}>
  <div class="form-grid">
    <Field
      label="名称"
      hint={editing ? '名字不能改：任务是按名字挂规则的' : undefined}
      error={fieldErr.name}
    >
      {#snippet control(p)}
        <input {...p} bind:value={name} placeholder="no-restart" spellcheck="false" disabled={editing !== null} />
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
    <Field label="规则文本" error={fieldErr.text} wide>
      {#snippet control(p)}
        <textarea {...p} bind:value={text} rows="4" class="prose" placeholder="不要重启任何服务。需要重启时先报告，等人确认。"></textarea>
      {/snippet}
    </Field>
  </div>
  {#snippet footer()}
    <button class="btn-ghost" onclick={onclose} disabled={busy}>取消</button>
    <button class="btn-primary" onclick={save} disabled={busy || !name || !text}>
      {editing ? '保存' : '添加规则'}
    </button>
  {/snippet}
</Modal>

{#if !loaded}
  <div class="card"><Loading rows={3} /></div>
{:else if items.length}
  <div class="card flush">
    <table>
      <thead>
        <tr><th>名称</th><th>规则文本</th><th>范围</th><th class="act"></th></tr>
      </thead>
      <tbody>
        {#each items as rule (rule.id)}
          <tr class:off={!rule.enabled} class:on={editing?.id === rule.id}>
            <td class="mono name">{rule.name}</td>
            <td class="muted text">{bodyText(rule)}</td>
            <td>
              <span class="tag" class:accent={rule.scope === 'global'}>
                {rule.scope === 'global' ? '全局' : '按任务挂载'}
              </span>
            </td>
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
  <Empty title="还没有软规则" hint="软规则让模型少走弯路。真正要拦住的事情去「硬策略」页签配。">
    {#snippet action()}
      <button class="btn-primary" onclick={onnew}>新增软规则</button>
    {/snippet}
  </Empty>
{/if}

<style>
  .name {
    font-weight: 500;
  }
  .text {
    max-width: 60ch;
  }
</style>
