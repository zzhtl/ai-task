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
  import { api, ApiFailure, describeError, ignoreForbidden } from '$api/client';
  import { listHosts, listRules, listSkills, type Host, type Rule, type Skill } from '$api/models';
  import type { DagSpec } from '$api/types/DagSpec';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast } from '$lib/ui/toast.svelte';
  import { DEFAULTS, fromSpec, newStep, toSpec, type Composition } from '$lib/tasks/compose';
  import StepEditor from '$lib/tasks/StepEditor.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import { session } from '$lib/auth/session.svelte';

  const editingId = $derived(page.url.searchParams.get('id'));

  let name = $state('');
  let description = $state('');
  let comp = $state<Composition>({ steps: [newStep('ai')], budgetUsd: DEFAULTS.budgetUsd });
  let etag = $state<string | null>(null);
  let enabled = $state(true);
  let error = $state<string | null>(null);
  let fieldErrors = $state<Array<{ field: string; message: string }>>([]);

  interface Snapshot {
    name: string;
    description: string;
    picked: string[];
    enabled: boolean;
    comp: Composition;
  }

  /** 先比便宜的，再比贵的。返回 true 表示有改动。 */
  function changed(a: Snapshot, b: Snapshot): boolean {
    if (
      a.name !== b.name ||
      a.description !== b.description ||
      a.enabled !== b.enabled ||
      a.picked.length !== b.picked.length ||
      a.comp.steps.length !== b.comp.steps.length
    ) {
      return true;
    }
    // 步骤数和标量都一样，才值得付一次序列化的钱
    return JSON.stringify(a) !== JSON.stringify(b);
  }

  /**
   * `TaskDetail` 用了 serde(flatten)，`summary` 的字段是**平铺在顶层**的。
   * 照着生成类型写 `task.summary.name` 拿到的是 undefined，保存时会把名字清空。
   */
  type TaskDetailFlat = {
    name: string;
    description?: string | null;
    spec: DagSpec;
    rules?: string[] | null;
    enabled: boolean;
  };
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
      .catch(ignoreForbidden);
    listSkills()
      .then((s) => (skills = s))
      .catch(ignoreForbidden);
    listRules()
      .then((r) => (rules = r.filter((x) => x.scope === 'task' && x.enabled)))
      .catch(ignoreForbidden);
  });

  $effect(() => {
    const id = editingId;
    if (!id) {
      ready = true;
      return;
    }
    // 走 api() 而不是裸 fetch。之前这里绕开封装只为读一个 ETag，
    // 代价是丢掉了 15 秒超时、结构化错误和 401 处理——现在 onHeaders 把头交出来。
    api<TaskDetailFlat>(`/api/v1/tasks/${id}`, { onHeaders: (h) => (etag = h.get('etag')) })
      .then((task) => {
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
  /** viewer 可以打开这一页看，但保存不了：早点说，别等点了保存才弹 403。 */
  const canOperate = $derived(session.can('operator'));
  const canSave = $derived(!busy && !notLinear && canOperate && name.trim() !== '' && stepsReady);
  const missing = $derived.by(() => {
    if (!canOperate) return '需要 operator 权限才能保存';
    if (name.trim() === '') return '还没起名字';
    if (!stepsReady) return '有步骤还是空的';
    return null;
  });

  /**
   * 有没有改过东西。用来在离开前拦一下——一页提示词写了十分钟，误点侧栏就没了。
   * 拿加载完成那一刻的快照做基线，和当前内容比；直接监听"变过没有"会把
   * 加载本身也算成一次修改。
   */
  // **不要每次敲键都把整个编排 JSON.stringify 一遍。**
  // 提示词是逐字符输入的，而这里序列化的是全部步骤 + 规则 + 预算——
  // 一个长 prompt 打字时每个字符都要重新序列化一次整棵树。
  //
  // 改成先比标量和长度（绝大多数改动在这一步就分出来了），
  // 只有它们都相等时才落到深比较上。
  let baseline = $state<Snapshot | null>(null);
  const snapshot = $derived<Snapshot>({ name, description, picked, enabled, comp });
  $effect(() => {
    if (ready && baseline === null) baseline = untrack(() => structuredClone(snapshot));
  });
  const dirty = $derived(baseline !== null && changed(baseline, snapshot));

  /** 被拦下来的那次跳转。确认之后用它继续走。 */
  let pendingNav = $state<URL | null>(null);
  /** 用户已经点过"仍然离开"：这一次 goto 不要再拦，否则就出不去了。 */
  let leaving = false;

  // beforeNavigate 是同步的，而 <dialog> 的确认是异步的，所以这里一律先 cancel，
  // 等用户点了确认再用 pendingNav 重新发起同一次跳转。
  beforeNavigate((nav) => {
    if (!dirty || saved || busy || leaving) return;
    // 关标签页 / 关浏览器只有原生提示这一条路，cancel() 正是触发它的方式。
    // 之前这一档被整个排除在外，于是误关标签页会静默丢掉整页提示词。
    if (nav.type === 'leave') {
      nav.cancel();
      return;
    }
    nav.cancel();
    pendingNav = nav.to?.url ?? null;
  });

  function leaveAnyway() {
    const to = pendingNav;
    leaving = true;
    if (to) void goto(to);
  }

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
      if (e instanceof ApiFailure && e.status === 412) {
        // 打开之后任务被改过（别人，或者你自己在另一个标签页）。直接覆盖会把
        // 对方的改动悄悄冲掉，所以不保存，让人选
        conflict = true;
      } else if (e instanceof ApiFailure) {
        error = e.message;
        fieldErrors = e.body.details ?? [];
      } else {
        error = describeError(e);
      }
    } finally {
      busy = false;
    }
  }

  /** 保存时撞上了别人的改动（412）。 */
  let conflict = $state(false);

  /** 丢掉这一页的修改，重新读最新版本。 */
  function reloadLatest() {
    saved = true; // 不再拦离开：这正是用户选的
    location.reload();
  }

  /** 这一页的修改另存成一个新任务，原任务不动。 */
  async function saveAsNew() {
    if (!spec) return;
    busy = true;
    try {
      const created = await api<{ id: string }>('/api/v1/tasks', {
        method: 'POST',
        body: {
          name: `${name.trim()} · 我的修改`,
          description: description || null,
          spec,
          rules: picked,
          enabled
        },
        idempotencyKey: crypto.randomUUID()
      });
      saved = true;
      conflict = false;
      toast('已另存为新任务，原任务没动');
      await goto(`/tasks/${created.id}`);
    } catch (e) {
      conflict = false;
      error = describeError(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal open={conflict} title="这个任务在你编辑期间被改过" onclose={() => (conflict = false)}>
  <p>直接保存会把对方的改动覆盖掉，所以这次没有保存。你这一页的修改还在。</p>
  <p class="faint small">可以另存为一个新任务，或者丢掉这一页的修改、重新加载最新版本再改。</p>
  {#snippet footer()}
    <button class="btn-ghost" onclick={() => (conflict = false)} disabled={busy}>先不处理</button>
    <button class="btn-danger" onclick={reloadLatest} disabled={busy}>丢掉修改并重新加载</button>
    <button class="btn-primary" onclick={saveAsNew} disabled={busy}>另存为新任务</button>
  {/snippet}
</Modal>

<Confirm
  open={pendingNav !== null}
  title="有未保存的改动"
  danger
  confirmText="仍然离开"
  onconfirm={leaveAnyway}
  onclose={() => (pendingNav = null)}
>
  <p>这一页的编辑还没保存，离开后会丢掉。</p>
</Confirm>

<PageHeader
  title={editingId ? '编辑任务' : '新建任务'}
  crumb={editingId ? name || '任务' : '任务'}
  crumbHref={editingId ? `/tasks/${editingId}` : '/tasks'}
>
  {#snippet sub()}
    {#if editingId}<span>保存会生成新版本，历史执行不受影响</span>{/if}
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
  <section class="card basics">
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
  .basics {
    margin-bottom: var(--s4);
  }
  .two {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 340px;
    gap: var(--s4);
    align-items: start;
  }
  @media (max-width: 1280px) {
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
    font-size: var(--t-xs);
  }
  .rules .tag {
    margin-left: var(--s1);
  }
</style>
