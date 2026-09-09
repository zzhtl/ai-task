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
  import { untrack } from 'svelte';
  import { page } from '$app/state';
  import { goto, beforeNavigate } from '$app/navigation';
  import { api, ApiFailure, describeError } from '$api/client';
  import { listHosts, listRules, listSkills, type Host, type Rule, type Skill } from '$api/models';
  import type { DagSpec } from '$api/types/DagSpec';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast } from '$lib/ui/toast.svelte';
  import { DEFAULTS, fromSpec, newStep, toSpec, type Composition } from '$lib/tasks/compose';
  import StepEditor from '$lib/tasks/StepEditor.svelte';

  const editingId = $derived(page.url.searchParams.get('id'));

  let name = $state('');
  let description = $state('');
  let comp = $state<Composition>({ steps: [newStep('ai')], budgetUsd: DEFAULTS.budgetUsd });
  let etag = $state<string | null>(null);
  let enabled = $state(true);
  let error = $state<string | null>(null);
  let fieldErrors = $state<Array<{ field: string; message: string }>>([]);
  let busy = $state(false);
  let ready = $state(false);
  let hosts = $state<Host[]>([]);
  let skills = $state<Skill[]>([]);
  /** 可以挂到这个任务上的规则。全局规则不在这里——它们本来就对所有任务生效。 */
  let rules = $state<Rule[]>([]);
  let picked = $state<string[]>([]);
  /** 打开的是一份步骤列表表示不了的编排。只能看，不能在这儿改。 */
  let notLinear = $state(false);
  /** 保存成功后置位，离开时不再拦。 */
  let saved = $state(false);

  $effect(() => {
    // 主机 / 规则是 admin 才能读；operator 建任务时读不到不该报错
    listHosts()
      .then((h) => (hosts = h))
      .catch(() => {});
    listSkills()
      .then((s) => (skills = s))
      .catch(() => {});
    listRules()
      .then((r) => (rules = r.filter((x) => x.scope === 'task' && x.enabled)))
      .catch(() => {});
  });

  $effect(() => {
    const id = editingId;
    if (!id) {
      ready = true;
      return;
    }
    fetch(`/api/v1/tasks/${id}`, { headers: { accept: 'application/json' } })
      .then(async (r) => {
        if (!r.ok) throw new Error(`加载任务失败：${r.status}`);
        etag = r.headers.get('etag');
        // TaskDetail 用了 serde(flatten)，summary 的字段是**平铺在顶层**的
        // ——读 task.summary.name 拿到的是 undefined，保存时会把名字清空
        const task: {
          name: string;
          description?: string | null;
          spec: DagSpec;
          rules?: string[] | null;
          enabled: boolean;
        } = await r.json();
        name = task.name;
        description = task.description ?? '';
        enabled = task.enabled;
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
      .catch((e) => (error = describeError(e)))
      .finally(() => (ready = true));
  });

  const spec = $derived<DagSpec | null>(notLinear ? null : toSpec(comp));
  /** 每个非审批步骤都得有内容，否则保存出去的是一个空壳。 */
  const stepsReady = $derived(
    comp.steps.length > 0 && comp.steps.every((s) => s.kind === 'approval' || s.body.trim() !== '')
  );
  const canSave = $derived(!busy && !notLinear && name.trim() !== '' && stepsReady);
  const missing = $derived.by(() => {
    if (name.trim() === '') return '还没起名字';
    if (!stepsReady) return '有步骤还是空的';
    return null;
  });

  /**
   * 有没有改过东西。用来在离开前拦一下——一页提示词写了十分钟，误点侧栏就没了。
   * 拿加载完成那一刻的快照做基线，和当前内容比；直接监听"变过没有"会把
   * 加载本身也算成一次修改。
   */
  const fingerprint = $derived(JSON.stringify({ name, description, picked, enabled, comp }));
  let baseline = $state<string | null>(null);
  $effect(() => {
    if (ready && baseline === null) baseline = untrack(() => fingerprint);
  });
  const dirty = $derived(baseline !== null && fingerprint !== baseline);
  beforeNavigate((nav) => {
    if (dirty && !saved && !busy && nav.type !== 'leave') {
      if (!confirm('有没保存的改动，确定离开？')) nav.cancel();
    }
  });

  async function save() {
    if (!spec) return;
    busy = true;
    error = null;
    fieldErrors = [];
    try {
      const body = { name, description: description || null, spec, rules: picked, enabled };
      if (editingId) {
        await api(`/api/v1/tasks/${editingId}`, {
          method: 'PUT',
          body,
          ifMatch: etag ?? undefined
        });
        saved = true;
        toast('已保存为新版本');
        await goto(`/tasks/${editingId}`);
      } else {
        const created = await api<{ id: string }>('/api/v1/tasks', {
          method: 'POST',
          body,
          idempotencyKey: crypto.randomUUID()
        });
        saved = true;
        toast(`已创建「${name}」`);
        await goto(`/tasks/${created.id}`);
      }
    } catch (e) {
      if (e instanceof ApiFailure) {
        error = e.message;
        fieldErrors = e.body.details ?? [];
      } else {
        error = describeError(e);
      }
    } finally {
      busy = false;
    }
  }
</script>

<PageHeader
  title={editingId ? '编辑任务' : '新建任务'}
  crumb={editingId ? name || '任务' : '任务'}
  crumbHref={editingId ? `/tasks/${editingId}` : '/tasks'}
>
  {#snippet sub()}
    {#if editingId}
      <span>保存会产生一个<b>新版本</b>，老版本保留——历史 run 绑的是版本快照。</span>
    {:else}
      <span>按顺序把步骤列出来。每一步说清楚做什么、在哪台机器上、谁来做。</span>
    {/if}
  {/snippet}
  {#snippet actions()}
    {#if missing && !notLinear}<span class="faint small">{missing}</span>{/if}
    <a class="btn btn-ghost" href={editingId ? `/tasks/${editingId}` : '/tasks'}>取消</a>
    <button class="btn-primary" onclick={save} disabled={!canSave}>
      {busy ? '保存中…' : editingId ? '保存为新版本' : '创建任务'}
    </button>
  {/snippet}
</PageHeader>

{#if error}
  <div class="banner">
    <div>
      {error}
      {#if fieldErrors.length}
        <ul>
          {#each fieldErrors as fe (fe.field + fe.message)}
            <li><span class="mono">{fe.field}</span>：{fe.message}</li>
          {/each}
        </ul>
      {/if}
    </div>
  </div>
{/if}

{#if !ready}
  <div class="card"><Loading rows={4} /></div>
{:else if notLinear}
  <div class="callout warn">
    这个任务的编排里有分支、并行或 map 这类结构，不是一条直线，步骤编辑器改不了它。
    <a href="/tasks/{editingId}">回任务详情</a>看步骤。
  </div>
{:else}
  <section class="card meta">
    <div class="form-grid">
      <label class="field">
        任务名称
        <input bind:value={name} placeholder="每天凌晨巡检 prod" />
      </label>
      <label class="field">
        描述
        <input bind:value={description} placeholder="给同事看的一句话，可留空" />
      </label>
    </div>
  </section>

  <div class="two">
    <section class="steps">
      <h2 class="section-title">步骤</h2>
      <StepEditor bind:comp {hosts} {skills} />
    </section>

    <aside class="side">
      <section class="card">
        <header class="card-head"><h2>整体</h2></header>
        <label class="field">
          花费上限（美元）
          <input bind:value={comp.budgetUsd} placeholder="1.000000" inputmode="decimal" />
          <span class="hint">
            累计花到这个数就不再启动新步骤，run 标 <code>budget_exceeded</code>。
            已经花掉的钱拦不住。
          </span>
        </label>
        {#if editingId}
          <label class="check">
            <input type="checkbox" bind:checked={enabled} />
            <span>启用<span class="faint">（停用后定时不触发、也不能手动运行）</span></span>
          </label>
        {/if}
      </section>

      <section class="card">
        <header class="card-head">
          <h2>挂上规则</h2>
          <span class="sub">全局规则自动生效，这里只列按任务挂载的</span>
        </header>
        {#if rules.length}
          <div class="rules">
            {#each rules as rule (rule.id)}
              <label class="check">
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
                  <span class="tag" class:warn={rule.kind === 'policy'}>{rule.kind === 'policy' ? '硬策略' : '软规则'}</span>
                  <em>
                    {'effect' in rule.spec ? `${rule.spec.effect} · ${rule.spec.reason}` : (rule.spec.text ?? '')}
                  </em>
                </span>
              </label>
            {/each}
          </div>
        {:else}
          <p class="faint small">
            还没有"按任务挂载"的规则。去<a href="/rules">规则页</a>建一条，作用范围选「按任务挂载」。
          </p>
        {/if}
      </section>
    </aside>
  </div>
{/if}

<style>
  .meta {
    margin-bottom: var(--s4);
  }
  .two {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 340px;
    gap: var(--s4);
    align-items: start;
  }
  @media (max-width: 1100px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
  .section-title {
    margin-bottom: var(--s3);
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    position: sticky;
    top: var(--s4);
  }
  .side .card {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  .rules {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .rules em {
    font-style: normal;
    color: var(--fg-faint);
    display: block;
    font-size: 0.76rem;
  }
  .rules .tag {
    margin-left: var(--s1);
  }
</style>
