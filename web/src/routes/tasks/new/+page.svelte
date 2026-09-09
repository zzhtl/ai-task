<script lang="ts">
  /**
   * 新建 / 编辑任务。
   *
   * **只有一种编排方式：按顺序列步骤。**这里没有 YAML，也没有"高级模式"。
   * 一个团队里能看懂 DAG YAML 的是少数人，而需要配任务的是所有人；把 YAML
   * 摆在这儿的代价是大多数人根本不敢动这个页面。
   *
   * 步骤列表表示不了的结构（分支、并行、map）在这个界面里就是表示不了——
   * 遇到这种任务老实说，不硬塞进步骤列表：硬塞会让人以为自己编辑的是全部，
   * 然后保存时悄悄丢掉一半。
   */
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { api, ApiFailure } from '$api/client';
  import type { DagSpec } from '$api/types/DagSpec';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import { DEFAULTS, fromSpec, newStep, toSpec, type Composition } from '$lib/tasks/compose';
  import StepEditor from '$lib/tasks/StepEditor.svelte';

  interface AiCli {
    name: string;
    path: string;
    version: string | null;
  }
  interface Host {
    id: string;
    name: string;
    address: string;
    tags: string[];
    cgroup_mode: string | null;
    degraded: boolean;
    ai_clis: AiCli[];
  }
  interface Skill {
    name: string;
    description: string;
  }
  interface Rule {
    id: string;
    name: string;
    kind: 'prompt' | 'policy';
    scope: 'global' | 'task';
    spec: Record<string, unknown>;
    enabled: boolean;
  }

  const editingId = $derived(page.url.searchParams.get('id'));

  let name = $state('');
  let description = $state('');
  let comp = $state<Composition>({ steps: [newStep('ai')], budgetUsd: DEFAULTS.budgetUsd });
  let etag = $state<string | null>(null);
  let error = $state<string | null>(null);
  let fieldErrors = $state<Array<{ field: string; message: string }>>([]);
  let busy = $state(false);
  let hosts = $state<Host[]>([]);
  let skills = $state<Skill[]>([]);
  /** 可以挂到这个任务上的规则。全局规则不在这里——它们本来就对所有任务生效。 */
  let rules = $state<Rule[]>([]);
  let picked = $state<string[]>([]);
  /** 打开的是一份步骤列表表示不了的编排。只能看，不能在这儿改。 */
  let notLinear = $state(false);

  $effect(() => {
    api<{ items: Host[] }>('/api/v1/hosts')
      .then((p) => (hosts = p.items))
      // 主机是 admin 才能读；operator 建任务时读不到不该报错
      .catch(() => {});
    api<{ items: Skill[] }>('/api/v1/skills')
      .then((p) => (skills = p.items))
      .catch(() => {});
    api<{ items: Rule[] }>('/api/v1/rules')
      .then((p) => (rules = p.items.filter((r) => r.scope === 'task' && r.enabled)))
      // 规则是 admin 才能读；operator 建任务时读不到不该报错
      .catch(() => {});
  });

  $effect(() => {
    const id = editingId;
    if (!id) return;
    fetch(`/api/v1/tasks/${id}`, { headers: { accept: 'application/json' } })
      .then(async (r) => {
        etag = r.headers.get('etag');
        // TaskDetail 用了 serde(flatten)，summary 的字段是**平铺在顶层**的
        // ——读 task.summary.name 拿到的是 undefined，保存时会把名字清空
        const task: {
          name: string;
          description?: string | null;
          spec: DagSpec;
          rules?: string[] | null;
        } = await r.json();
        name = task.name;
        description = task.description ?? '';
        // 编辑时保留原来挂着的规则。丢掉的话，改一次标题就把这个任务的
        // 约束全解除了——而界面上什么都不会提示
        picked = task.rules ?? [];
        const recovered = fromSpec(task.spec);
        if (recovered) {
          comp = recovered;
          notLinear = false;
        } else {
          notLinear = true;
        }
      })
      .catch((e) => (error = String(e)));
  });

  const spec = $derived<DagSpec | null>(notLinear ? null : toSpec(comp));
  /** 每个非审批步骤都得有内容，否则保存出去的是一个空壳。 */
  const stepsReady = $derived(
    comp.steps.length > 0 && comp.steps.every((s) => s.kind === 'approval' || s.body.trim() !== '')
  );
  const canSave = $derived(!busy && !notLinear && name.trim() !== '' && stepsReady);

  async function save() {
    if (!spec) return;
    busy = true;
    error = null;
    fieldErrors = [];
    try {
      const body = { name, description: description || null, spec, rules: picked, enabled: true };
      if (editingId) {
        await api(`/api/v1/tasks/${editingId}`, {
          method: 'PUT',
          body,
          ifMatch: etag ?? undefined
        });
        await goto(`/tasks/${editingId}`);
      } else {
        const created = await api<{ id: string }>('/api/v1/tasks', { method: 'POST', body });
        await goto(`/tasks/${created.id}`);
      }
    } catch (e) {
      if (e instanceof ApiFailure) {
        error = e.message;
        fieldErrors =
          (e.body as { details?: Array<{ field: string; message: string }> }).details ?? [];
      } else {
        error = String(e);
      }
    } finally {
      busy = false;
    }
  }
</script>

<PageHeader title={editingId ? '编辑任务' : '新建任务'} crumb="← 任务" crumbHref="/tasks">
  {#snippet sub()}
    {#if editingId}
      <span>保存会产生一个<b>新版本</b>，老版本保留——历史 run 绑的是版本快照。</span>
    {:else}
      <span>按顺序把步骤列出来。每一步说清楚做什么、在哪台机器上、谁来做。</span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <button class="btn-primary" onclick={save} disabled={!canSave}>
      {editingId ? '保存为新版本' : '创建任务'}
    </button>
  {/snippet}
</PageHeader>

{#if notLinear}
  <p class="bad">
    这个任务的编排里有分支、并行或 map 这类结构，不是一条直线，步骤编辑器改不了它。
    <a href="/tasks/{editingId}">回任务详情</a>看完整的编排图。
  </p>
{:else}
  <div class="meta">
    <label class="field">
      任务名称
      <input bind:value={name} placeholder="每天凌晨巡检 prod" />
    </label>
    <label class="field">
      描述（可留空）
      <input bind:value={description} placeholder="给同事看的一句话" />
    </label>
  </div>

  <div class="two">
    <section>
      <StepEditor bind:comp {hosts} {skills} />
    </section>

    <section class="card side">
      <h2>整体</h2>
      <label class="field">
        花费上限（美元）
        <input bind:value={comp.budgetUsd} placeholder="1.000000" />
      </label>
      <p class="hint">
        累计花到这个数就不再启动新步骤，run 标 <code>budget_exceeded</code>。
        已经花掉的钱拦不住——那笔钱真的花了。
      </p>

      <div class="rules">
        <span class="hint">挂上这些规则</span>
        {#if rules.length}
          {#each rules as rule (rule.id)}
            <label class="pick">
              <input
                type="checkbox"
                checked={picked.includes(rule.name)}
                onchange={(e) => {
                  picked = e.currentTarget.checked
                    ? [...picked, rule.name]
                    : picked.filter((n) => n !== rule.name);
                }}
              />
              <span>
                <b class="mono">{rule.name}</b>
                <span class="tag">{rule.kind === 'policy' ? '硬策略' : '软规则'}</span>
                <em>
                  {'effect' in rule.spec ? `${rule.spec.effect} · ${rule.spec.reason}` : (rule.spec.text ?? '')}
                </em>
              </span>
            </label>
          {/each}
        {:else}
          <p class="hint">
            还没有"按任务挂载"的规则。全局规则对所有任务自动生效，不用在这里勾。
            去<a href="/rules">规则页</a>建一条。
          </p>
        {/if}
      </div>
    </section>
  </div>
{/if}

{#if error}
  <p class="bad">{error}</p>
  <ul class="bad">
    {#each fieldErrors as fe (fe.field + fe.message)}
      <li><span class="mono">{fe.field}</span>：{fe.message}</li>
    {/each}
  </ul>
{/if}

<style>
  .meta {
    display: flex;
    gap: var(--s3);
    margin-bottom: var(--s4);
  }
  .meta label {
    flex: 1;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 360px;
    gap: var(--s4);
    align-items: start;
  }
  @media (max-width: 1000px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    position: sticky;
    top: var(--s4);
  }
  .side h2 {
    margin-bottom: 0;
  }
  .rules {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding-top: var(--s2);
    border-top: 1px solid var(--line);
  }
  .pick {
    display: flex;
    gap: var(--s2);
    align-items: baseline;
    font-size: 0.8rem;
  }
  .pick em {
    font-style: normal;
    color: var(--fg-faint);
    display: block;
  }
  .hint {
    margin: 0;
    font-size: 0.76rem;
    color: var(--fg-faint);
    line-height: 1.5;
  }
  ul.bad {
    margin: var(--s1) 0 0;
    padding-left: 1.2rem;
    font-size: 0.83rem;
  }
</style>
