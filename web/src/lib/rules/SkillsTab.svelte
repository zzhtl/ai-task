<script lang="ts">
  /**
   * 技能包：开箱即用的上下文包，勾选后落到 run 工作目录的 `.claude/skills/`。
   *
   * 和规则/策略放在同一个页面下，是因为它们都是"给 AI 的约束与补给"；
   * 但它没有启停和优先级，表单和表格都和另外两个不一样，所以各自成组件。
   */
  import { api } from '$api/client';
  import type { Skill } from '$api/models';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import type { Act } from './types';

  let {
    items,
    loaded,
    busy,
    adding,
    fieldErr,
    act,
    onclose,
    onnew
  }: {
    items: Skill[];
    loaded: boolean;
    busy: boolean;
    adding: boolean;
    fieldErr: Record<string, string>;
    act: Act;
    onclose: () => void;
    onnew: () => void;
  } = $props();

  let name = $state('');
  let description = $state('');
  let body = $state('');

  $effect(() => {
    if (!adding) {
      name = '';
      description = '';
      body = '';
    }
  });

  const save = () =>
    act(async () => {
      await api('/api/v1/skills', { method: 'POST', body: { name, description, body } });
      onclose();
    }, '技能已导入').catch(() => {});
</script>

<div class="callout">
  <strong>技能是开箱即用的上下文包。</strong>
  勾选后会落到 run 工作目录的 <code>.claude/skills/</code>，渐进式披露由 CLI 自己完成
  ——只有 name 和 description 进上下文，正文按需加载。在任务的 AI 步骤里勾选即可。
</div>

<Modal open={adding} title="导入技能" size="lg" {onclose}>
  <div class="form-grid">
    <Field label="名称" error={fieldErr.name}>
      {#snippet control(p)}
        <input {...p} bind:value={name} placeholder="linux-perf" spellcheck="false" />
      {/snippet}
    </Field>
    <!-- 渐进式披露时模型只看得到描述。写不清楚等于这个技能不会被用上 -->
    <Field
      label="描述"
      hint="模型只凭这一句决定要不要加载它，写清楚适用场景"
      error={fieldErr.description}
      wide
    >
      {#snippet control(p)}
        <input {...p} bind:value={description} placeholder="Linux 性能排查：CPU / 内存 / IO / 网络的定位顺序" />
      {/snippet}
    </Field>
    <Field label="正文（Markdown，就是 SKILL.md 的内容）" error={fieldErr.body} wide>
      {#snippet control(p)}
        <textarea {...p} bind:value={body} rows="12" spellcheck="false"
          placeholder="## 定位顺序&#10;&#10;1. 先看整机负载：uptime、vmstat 1&#10;2. ..."></textarea>
      {/snippet}
    </Field>
  </div>
  {#snippet footer()}
    <button class="btn-ghost" onclick={onclose} disabled={busy}>取消</button>
    <button class="btn-primary" onclick={save} disabled={busy || !name || !description || !body}>
      导入
    </button>
  {/snippet}
</Modal>

{#if !loaded}
  <div class="card"><Loading rows={3} /></div>
{:else if items.length}
  <div class="card flush">
    <table>
      <thead><tr><th>名称</th><th>描述</th><th>版本</th><th>内容哈希</th></tr></thead>
      <tbody>
        {#each items as s (s.name)}
          <tr>
            <td class="mono name">{s.name}</td>
            <td class="muted text">{s.description}</td>
            <td class="faint">{s.version}</td>
            <td class="mono faint">{s.content_hash.slice(0, 12)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if !adding}
  <Empty
    title="还没有技能包"
    hint="技能是一份写给 AI 的操作手册：这类问题怎么查、公司内部的命令怎么敲。导入之后在任务的 AI 步骤里勾选。"
  >
    {#snippet action()}
      <button class="btn-primary" onclick={onnew}>导入技能</button>
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
