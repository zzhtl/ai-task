<script lang="ts">
  /**
   * 规则与策略。
   *
   * 这两样**语义完全不同**，界面上必须分开，不能只用一个 kind 字段区分：
   *
   * - 软规则注入 system prompt，只影响模型的**倾向**。模型可以不听。
   * - 硬策略在**工具调用边界**强制拦截。模型绕不过去。
   *
   * 把它们并排列成一张表，会让人以为「写一条规则就管住了」——那是这类系统
   * 最常见的致命误解。三个页签各自成组件，正是为了不让人顺手把表合并。
   *
   * 这里只留三样共享的东西：数据加载、统一的错误/提示处理（`act`）、
   * 以及删除确认。表单字段归各自的页签自己管。
   */
  import { api, describeError, fieldErrors } from '$api/client';
  import { listRules, listSkills, type Rule, type Skill } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import PolicyTab from '$lib/rules/PolicyTab.svelte';
  import PromptTab from '$lib/rules/PromptTab.svelte';
  import SkillsTab from '$lib/rules/SkillsTab.svelte';

  type Tab = 'policy' | 'prompt' | 'skills';

  let rules = $state<Rule[]>([]);
  let skills = $state<Skill[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let busy = $state(false);
  let tab = $state<Tab>('policy');
  let adding = $state(false);
  /** 后端 422 里按字段拆出来的错误（正则写不通是最常见的一种）。 */
  let fieldErr = $state<Record<string, string>>({});
  /** 正在改的那条规则。表单复用新增的那一套，只是提交走 PUT。 */
  let editing = $state<Rule | null>(null);

  async function load() {
    try {
      const [s, r] = await Promise.all([listSkills(), listRules()]);
      skills = s;
      rules = r;
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  /** 三个页签共用的动作包装：错误分诊、成功提示、重新加载都在这里。 */
  async function act(run: () => Promise<unknown>, done?: string) {
    busy = true;
    error = null;
    fieldErr = {};
    try {
      await run();
      await load();
      if (done) toast(done);
    } catch (e) {
      // 正则编译不过是这里最常见的失败，显示在模式框底下比丢进页头有用
      fieldErr = fieldErrors(e);
      error = Object.keys(fieldErr).length ? null : describeError(e);
      throw e;
    } finally {
      busy = false;
    }
  }

  function closeForm() {
    adding = false;
    editing = null;
    fieldErr = {};
  }

  function openNew() {
    editing = null;
    adding = true;
  }

  function openEdit(rule: Rule) {
    editing = rule;
    adding = true;
  }

  const toggle = (rule: Rule) =>
    act(
      () => api(`/api/v1/rules/${rule.id}/enabled`, { method: 'PUT', body: { enabled: !rule.enabled } }),
      rule.enabled ? `已停用 ${rule.name}` : `已启用 ${rule.name}`
    ).catch(() => {});

  let pendingDelete = $state<Rule | null>(null);
  const remove = (rule: Rule) =>
    act(() => api(`/api/v1/rules/${rule.id}`, { method: 'DELETE' }), `已删除 ${rule.name}`).catch(
      (e) => toastError(describeError(e))
    );

  /** 当前页签下的规则。软规则和硬策略语义不同，绝不能并成一张表。 */
  const kindRules = $derived(
    rules
      .filter((r) => r.kind === (tab === 'policy' ? 'policy' : 'prompt'))
      .sort((a, b) => b.priority - a.priority || a.name.localeCompare(b.name))
  );
  const policyCount = $derived(rules.filter((r) => r.kind === 'policy').length);
  const promptCount = $derived(rules.filter((r) => r.kind === 'prompt').length);

  $effect(() => {
    void load();
  });
  // 换页签时把没提交的表单收掉：留着上一个页签的半成品没有意义
  $effect(() => {
    void tab;
    closeForm();
    error = null;
  });

  const NEW_LABEL: Record<Tab, string> = {
    policy: '新增策略',
    prompt: '新增软规则',
    skills: '导入技能'
  };

  const shared = $derived({ busy, adding, editing, fieldErr, act, onclose: closeForm });
</script>

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
  title="删除「{pendingDelete?.name ?? ''}」？"
  danger
  confirmText="删除"
  {busy}
  onconfirm={() => {
    const rule = pendingDelete;
    pendingDelete = null;
    if (rule) void remove(rule);
  }}
>
  <p>
    删掉之后，历史执行记录里的规则指纹就对不上这条规则了；只是想暂时不生效的话，
    用<b>停用</b>。
  </p>
</Confirm>

<PageHeader title="规则与策略">
  {#snippet sub()}
    <span>约束 AI 行为的两层。它们的强度<b>不一样</b>。</span>
  {/snippet}
  {#snippet actions()}
    {#if session.can('admin') && !adding}
      <button class="btn-primary" onclick={openNew}>{NEW_LABEL[tab]}</button>
    {/if}
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <div class="callout danger">需要管理员权限：策略是这个系统的护栏本身。</div>
{:else}
  <div class="toolbar">
    <div class="seg">
      <button class:on={tab === 'policy'} onclick={() => (tab = 'policy')}>
        硬策略 <span class="count">{policyCount}</span>
      </button>
      <button class:on={tab === 'prompt'} onclick={() => (tab = 'prompt')}>
        软规则 <span class="count">{promptCount}</span>
      </button>
      <button class:on={tab === 'skills'} onclick={() => (tab = 'skills')}>
        技能 <span class="count">{skills.length}</span>
      </button>
    </div>
  </div>

  {#if error}<div class="banner">{error}</div>{/if}

  {#if tab === 'policy'}
    <PolicyTab
      {...shared}
      items={kindRules}
      {loaded}
      onedit={openEdit}
      ondelete={(rule) => (pendingDelete = rule)}
      ontoggle={toggle}
      onnew={openNew}
    />
  {:else if tab === 'prompt'}
    <PromptTab
      {...shared}
      items={kindRules}
      {loaded}
      onedit={openEdit}
      ondelete={(rule) => (pendingDelete = rule)}
      ontoggle={toggle}
      onnew={openNew}
    />
  {:else}
    <SkillsTab
      {...shared}
      items={skills}
      {loaded}
      onnew={openNew}
    />
  {/if}
{/if}
