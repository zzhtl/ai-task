<script lang="ts">
  /**
   * 新建 / 编辑 / 复制任务（`?id=` 编辑，`?from=` 复制）。
   *
   * **只有一种编排方式：按顺序列步骤。**这里没有 YAML，也没有"高级模式"。
   * 一个团队里能看懂 DAG YAML 的是少数人，而需要配任务的是所有人；把 YAML
   * 摆在这儿的代价是大多数人根本不敢动这个页面。
   *
   * 步骤列表表示不了的结构（分支、并行、map）在这个界面里就是表示不了——
   * 遇到这种任务老实说，不硬塞进步骤列表：硬塞会让人以为自己编辑的是全部，
   * 然后保存时悄悄丢掉一半。
   */
  import { tick, untrack } from 'svelte';
  import { page } from '$app/state';
  import { goto, beforeNavigate } from '$app/navigation';
  import { api, ApiFailure, describeError, ignoreForbidden } from '$api/client';
  import { invalidate } from '$api/resource.svelte';
  import { newIdempotencyKey } from '$api/runs';
  import { listHosts, listRules, listSkills, type Host, type Rule, type Skill } from '$api/models';
  import type { DagSpec } from '$api/types/DagSpec';
  import Loading from '$lib/ui/Loading.svelte';
  import Dropdown from '$lib/ui/Dropdown.svelte';
  import { toast } from '$lib/ui/toast.svelte';
  import { DEFAULTS, fromSpec, newStep, toSpec, type Composition } from '$lib/tasks/compose';
  import { problemsOf } from '$lib/tasks/problems';
  import { TEMPLATES, type TaskTemplate } from '$lib/tasks/templates';
  import StepEditor from '$lib/tasks/StepEditor.svelte';
  import DagView from '$lib/dag/DagView.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import { session } from '$lib/auth/session.svelte';

  const editingId = $derived(page.url.searchParams.get('id'));
  const fromId = $derived(page.url.searchParams.get('from'));

  let name = $state('');
  let description = $state('');
  let comp = $state<Composition>({ steps: [newStep('ai')], budgetUsd: DEFAULTS.budgetUsd });
  let etag = $state<string | null>(null);
  let enabled = $state(true);
  let error = $state<string | null>(null);
  let fieldErrors = $state<Array<{ field: string; message: string }>>([]);
  /** 编辑时原来的名字：面包屑上指回的是那个任务，不该跟着输入框变。 */
  let originalName = $state('');

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
  /** 可以开始编辑了：编辑 / 复制时是读完原任务，新建时是选完模板。 */
  let ready = $state(false);
  let hosts = $state<Host[]>([]);
  let skills = $state<Skill[]>([]);
  /** 可以挂到这个任务上的规则。全局规则不在这里——它们本来就对所有任务生效。 */
  let rules = $state<Rule[]>([]);
  let picked = $state<string[]>([]);
  /** 打开的是一份步骤列表表示不了的编排。只能看，不能在这儿改。 */
  let notLinear = $state(false);
  /** 那份编排本身。复制时原样带走：复制不需要改步骤，没理由因为编辑器画不了就不让复制。 */
  let rawSpec = $state<DagSpec | null>(null);
  /** 保存成功后置位，离开时不再拦。 */
  let saved = $state(false);
  let editor = $state<ReturnType<typeof StepEditor> | null>(null);
  let nameInput = $state<HTMLInputElement | null>(null);

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

  /** 把一个已有任务读进编辑器。编辑是改它本身，复制是拿它当新任务的底稿。 */
  function load(id: string, asCopy: boolean) {
    // 走 api() 而不是裸 fetch：要它的超时、结构化错误和 401 处理，ETag 由 onHeaders 交出来
    api<TaskDetailFlat>(`/api/v1/tasks/${id}`, { onHeaders: (h) => (etag = asCopy ? null : h.get('etag')) })
      .then((task) => {
        originalName = task.name;
        name = asCopy ? `${task.name} 副本` : task.name;
        description = task.description ?? '';
        enabled = asCopy ? true : task.enabled;
        // 保留原来挂着的规则。丢掉的话，改一次标题就把这个任务的约束全解除了
        picked = task.rules ?? [];
        const recovered = fromSpec(task.spec);
        if (recovered) {
          comp = recovered;
          notLinear = false;
        } else {
          notLinear = true;
          rawSpec = task.spec;
        }
      })
      .catch((e) => (error = describeError(e)))
      .finally(() => (ready = true));
  }

  $effect(() => {
    const id = editingId;
    const from = fromId;
    untrack(() => {
      if (id) load(id, false);
      else if (from) load(from, true);
    });
  });

  /** 新建：选一个起点。模板是 JSON 夹具，拷一份再用，别改到模块里那份。 */
  function useTemplate(t: TaskTemplate) {
    comp = t.spec
      ? (fromSpec(structuredClone(t.spec)) ?? { steps: [newStep('ai')], budgetUsd: DEFAULTS.budgetUsd })
      : { steps: [newStep('ai')], budgetUsd: DEFAULTS.budgetUsd };
    ready = true;
    void tick().then(() => nameInput?.focus());
  }

  /** 原样复制一份非直线编排：步骤不经过编辑器，只有名字要填。 */
  const copyingRaw = $derived(notLinear && !!fromId);
  const spec = $derived<DagSpec | null>(notLinear ? (copyingRaw ? rawSpec : null) : toSpec(comp));
  const problems = $derived(
    copyingRaw
      ? name.trim()
        ? []
        : [{ stepUid: null, level: 'error' as const, text: '还没起名字' }]
      : problemsOf(name, comp)
  );
  const errorCount = $derived(problems.filter((p) => p.level === 'error').length);
  /** viewer 可以打开这一页看，但保存不了：早点说，别等点了保存才弹 403。 */
  const canOperate = $derived(session.can('operator'));
  const canSave = $derived(!busy && spec !== null && canOperate && errorCount === 0);

  /**
   * 有没有改过东西。用来在离开前拦一下——一页提示词写了十分钟，误点侧栏就没了。
   * 拿能编辑那一刻的快照做基线，和当前内容比；直接监听"变过没有"会把加载本身也算成一次修改。
   */
  // **不要每次敲键都把整个编排 JSON.stringify 一遍。**先比标量和长度（绝大多数改动
  // 在这一步就分出来了），只有它们都相等时才落到深比较上。
  let baseline = $state<Snapshot | null>(null);
  const snapshot = $derived<Snapshot>({ name, description, picked, enabled, comp });
  $effect(() => {
    if (ready && baseline === null) baseline = untrack(() => $state.snapshot(snapshot) as Snapshot);
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

  /** 这一页"新建"这个意图的幂等键：失败重试复用它，服务端认得出是同一次，不会建出两个。 */
  const createKey = newIdempotencyKey();

  async function save() {
    if (!spec) return;
    busy = true;
    error = null;
    fieldErrors = [];
    try {
      const body = { name: name.trim(), description: description.trim() || null, spec, rules: picked, enabled };
      if (editingId) {
        await api(`/api/v1/tasks/${editingId}`, { method: 'PUT', body, ifMatch: etag ?? undefined });
        saved = true;
        invalidate('tasks');
        toast('已保存为新版本');
        await goto(`/tasks/${editingId}`);
      } else {
        const created = await api<{ id: string }>('/api/v1/tasks', {
          method: 'POST',
          body,
          idempotencyKey: createKey
        });
        saved = true;
        invalidate('tasks');
        toast(`已创建「${name.trim()}」`);
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

  const saveAsNewKey = newIdempotencyKey();
  /** 这一页的修改另存成一个新任务，原任务不动。 */
  async function saveAsNew() {
    if (!spec) return;
    busy = true;
    try {
      const created = await api<{ id: string }>('/api/v1/tasks', {
        method: 'POST',
        body: {
          name: `${name.trim()} · 我的修改`,
          description: description.trim() || null,
          spec,
          rules: picked,
          enabled
        },
        idempotencyKey: saveAsNewKey
      });
      saved = true;
      conflict = false;
      invalidate('tasks');
      toast('已另存为新任务，原任务没动');
      await goto(`/tasks/${created.id}`);
    } catch (e) {
      conflict = false;
      error = describeError(e);
    } finally {
      busy = false;
    }
  }

  const cancelHref = $derived(editingId ? `/tasks/${editingId}` : fromId ? `/tasks/${fromId}` : '/tasks');
  const choosing = $derived(!editingId && !fromId && !ready);
</script>

<svelte:head>
  <title>{editingId ? `编辑 ${originalName}` : fromId ? '复制任务' : '新建任务'} · ai-task</title>
</svelte:head>

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

<nav class="crumbs" aria-label="位置">
  <a href="/tasks">任务</a>
  <span aria-hidden="true">/</span>
  {#if editingId}
    <a href={`/tasks/${editingId}`}>{originalName || '…'}</a>
    <span aria-hidden="true">/</span>
    <span>编辑</span>
  {:else if fromId}
    <span>复制「{originalName || '…'}」</span>
  {:else}
    <span>新建</span>
  {/if}
</nav>

{#if choosing}
  <section class="start">
    <h1>新建任务</h1>
    <p class="lead">从哪里开始？选完还能随便改。</p>
    <div class="templates">
      {#each TEMPLATES as t (t.id)}
        <button class="tpl" onclick={() => useTemplate(t)}>
          <b>{t.name}</b>
          <span class="tpl-hint">{t.hint}</span>
          {#if t.spec}
            <ol class="tpl-steps">
              {#each t.spec.nodes as n (n.key)}<li>{n.name}</li>{/each}
            </ol>
          {/if}
        </button>
      {/each}
    </div>
  </section>
{:else if !ready}
  <div class="card"><Loading rows={4} /></div>
{:else if notLinear && !copyingRaw}
  <div class="callout warn">
    这个任务的编排里有分支、并行或 map 这类结构，不是一条直线，步骤编辑器改不了它。
    <a href="/tasks/{editingId}">回任务详情</a>看步骤。
  </div>
{:else}
  <div class="bar">
    <div class="bar-main">
      <input
        bind:this={nameInput}
        class="name-input"
        bind:value={name}
        placeholder="给任务起个名字，比如「每天凌晨巡检 prod」"
        aria-label="任务名称"
      />
      {#if problems.length}
        <Dropdown
          label={errorCount ? `${errorCount} 个问题要先改` : `${problems.length} 条提醒`}
          triggerClass="btn-ghost btn-sm {errorCount ? 'has-errors' : ''}"
        >
          {#each problems as p (p.text)}
            <button
              onclick={() => (p.stepUid ? editor?.reveal(p.stepUid) : nameInput?.focus())}
              class:danger={p.level === 'error'}
            >
              {p.text}
              <span class="hint">{p.level === 'error' ? '改了才能保存' : '只是提醒，不影响保存'}</span>
            </button>
          {/each}
        </Dropdown>
      {/if}
      <span class="spacer"></span>
      {#if dirty && !saved}<span class="dirty" title="有没保存的改动">未保存</span>{/if}
      <a class="btn btn-ghost" href={cancelHref}>取消</a>
      <button
        class="btn-primary"
        onclick={save}
        disabled={!canSave}
        title={!canOperate ? '需要 operator 权限才能保存' : errorCount ? '先把问题改掉' : undefined}
      >
        {busy ? '保存中…' : editingId ? '保存为新版本' : '创建任务'}
      </button>
    </div>
    <input
      class="desc-input"
      bind:value={description}
      placeholder="一句话说明这个任务做什么，给同事看（可留空）"
      aria-label="任务描述"
    />
  </div>

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

  <div class="two">
    <section class="steps">
      {#if copyingRaw && rawSpec}
        <div class="callout">
          这个编排有分支、并行或 map 这类结构，步骤编辑器改不了它；复制会原样照搬，建好之后要改只能走接口。
        </div>
        <div class="card"><DagView spec={rawSpec} /></div>
      {:else}
        <StepEditor bind:this={editor} bind:comp {hosts} {skills} {problems} />
      {/if}
    </section>

    <aside class="side">
      <section class="card">
        <header class="card-head"><h2>整体</h2></header>
        {#if !copyingRaw}
        <label class="field">
          花费上限（美元）
          <input bind:value={comp.budgetUsd} placeholder="1.000000" inputmode="decimal" />
          <span class="hint">
            累计花到这个数就不再启动新步骤，run 标 <code>budget_exceeded</code>。已经花掉的钱拦不住。
          </span>
        </label>
        {:else}
          <p class="hint">花费上限照原任务：{rawSpec?.budget_usd ? `$${rawSpec.budget_usd}` : '不设'}。</p>
        {/if}
        {#if editingId}
          <label class="check">
            <input type="checkbox" bind:checked={enabled} />
            <span>启用<span class="faint">（停用后定时不触发、也不能手动运行）</span></span>
          </label>
        {/if}
        {#if editingId}
          <p class="hint">保存会生成新版本，已经跑过的执行不受影响。</p>
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
  .crumbs {
    display: flex;
    gap: var(--s1);
    align-items: center;
    font-size: var(--t-sm);
    color: var(--fg-faint);
    margin-bottom: var(--s2);
  }
  .crumbs a {
    color: var(--fg-dim);
  }

  .start h1 {
    margin: 0;
  }
  .start .lead {
    margin: var(--s1) 0 var(--s4);
  }
  .templates {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
    gap: var(--s3);
  }
  .tpl,
  :global(:root[data-theme='light']) .tpl {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    justify-content: flex-start;
    gap: var(--s2);
    height: auto;
    padding: var(--s4);
    text-align: left;
    font-weight: 400;
    border: 1px solid var(--line);
    border-radius: var(--r3);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
    color: var(--fg);
    white-space: normal;
    /* 全局按钮是单行的行高，卡片里好几行字会挤在一起 */
    line-height: 1.55;
  }
  .tpl:hover:not(:disabled) {
    border-color: var(--accent-border);
    background: var(--surface-1);
  }
  .tpl b {
    font-size: var(--t-md);
  }
  .tpl-hint {
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .tpl-steps {
    margin: 0;
    padding-left: 1.2rem;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }

  /* 顶部粘性栏：往下翻着改第七步的时候，保存和问题数还在眼前 */
  .bar {
    position: sticky;
    top: 0;
    z-index: var(--z-sticky);
    margin: 0 calc(-1 * var(--s3)) var(--s4);
    padding: var(--s2) var(--s3) var(--s3);
    background: var(--bg);
    border-bottom: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    gap: var(--s1);
  }
  .bar-main {
    display: flex;
    align-items: center;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  /* 带上父级：暗色主题的全局 input 规则特异性是 (0,2,1)，单个类压不过它 */
  .bar input.name-input {
    flex: 1 1 20rem;
    min-width: 0;
    height: auto;
    padding: var(--s1) 0;
    border: none;
    background: transparent;
    box-shadow: none;
    font-size: var(--t-xl);
    font-weight: 600;
    color: var(--fg);
  }
  .bar input.name-input:focus-visible,
  .bar input.desc-input:focus-visible {
    outline: none;
    box-shadow: none;
    border-bottom: 1px solid var(--accent);
    border-radius: 0;
  }
  .bar input.desc-input {
    height: auto;
    padding: 2px 0;
    border: none;
    background: transparent;
    box-shadow: none;
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .dirty {
    font-size: var(--t-xs);
    color: var(--warn-fg);
    white-space: nowrap;
  }
  .dirty::before {
    content: '●';
    margin-right: 4px;
  }
  .bar :global(.has-errors) {
    color: var(--bad-fg);
  }
  @media (max-width: 640px) {
    /* 手机上外壳自己有吸顶的顶栏，两条叠在一起会吃掉半屏 */
    .bar {
      position: static;
    }
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
  .side {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
    position: sticky;
    top: 7rem;
  }
  @media (max-width: 1280px) {
    .side {
      position: static;
    }
  }
  .side .card {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  .hint {
    margin: 0;
    font-size: var(--t-xs);
    color: var(--fg-faint);
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
