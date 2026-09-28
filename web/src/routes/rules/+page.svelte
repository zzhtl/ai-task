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
  import Tabs from '$lib/ui/Tabs.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import PolicyTab from '$lib/rules/PolicyTab.svelte';
  import PromptTab from '$lib/rules/PromptTab.svelte';
  import SkillsTab from '$lib/rules/SkillsTab.svelte';

  type Tab = 'policy' | 'prompt' | 'skills';

  /** 规则只有管理员能看；技能谁都能看，operator 能改。 */
  const isAdmin = $derived(session.can('admin'));
  const canEditSkills = $derived(session.can('operator'));

  let rules = $state<Rule[]>([]);
  let skills = $state<Skill[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let busy = $state(false);
  let tab = $state<Tab>(session.can('admin') ? 'policy' : 'skills');
  let adding = $state(false);
  /** 后端 422 里按字段拆出来的错误（正则写不通是最常见的一种）。 */
  let fieldErr = $state<Record<string, string>>({});
  /** 正在改的那条规则。表单复用新增的那一套，只是提交走 PUT。 */
  let editing = $state<Rule | null>(null);

  async function load() {
    try {
      // 非管理员拉 /rules 是 403：不拉，而不是拉了再把错误吞掉
      const [s, r] = await Promise.all([listSkills(), isAdmin ? listRules() : Promise.resolve([])]);
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
  // 规则页签只给管理员：登录信息晚到、或者从带 ?tab=policy 的链接进来时，落到技能上
  $effect(() => {
    if (!isAdmin && tab !== 'skills') tab = 'skills';
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

<PageHeader
  title="规则与技能"
  help="约束 AI 行为的两层强度不一样：软规则写进提示词，只影响模型的倾向；硬策略在工具调用边界强制拦截，模型绕不过去。技能是给 AI 的操作手册。"
>
  {#snippet actions()}
    {#if !adding && (tab === 'skills' ? canEditSkills : isAdmin)}
      <button class="btn-primary" onclick={openNew}>{NEW_LABEL[tab]}</button>
    {/if}
  {/snippet}
  {#snippet tabs()}
    {#if isAdmin}
      <Tabs
        bind:value={tab}
        tabs={[
          { id: 'policy', label: '硬策略', count: loaded ? policyCount : null },
          { id: 'prompt', label: '软规则', count: loaded ? promptCount : null },
          { id: 'skills', label: '技能', count: loaded ? skills.length : null }
        ]}
      />
    {/if}
  {/snippet}
</PageHeader>

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
    canEdit={canEditSkills}
    onchanged={() => void load()}
  />
{/if}
