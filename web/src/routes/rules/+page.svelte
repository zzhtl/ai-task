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
   * 最常见的致命误解。
   */
  import { api, describeError, fieldErrors } from '$api/client';
  import { listRules, listSkills, type Rule, type Skill } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import Icon from '$lib/ui/Icon.svelte';

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

  // 软规则
  let ruleName = $state('');
  let ruleText = $state('');
  /** 全局对所有任务生效；按任务挂载的只对显式勾上它的任务生效。 */
  let ruleGlobal = $state(false);

  // 硬策略
  let policyName = $state('');
  let policyTool = $state('Bash');
  let policyArg = $state('command');
  let policyPattern = $state('');
  let policyKind = $state<'regex' | 'glob' | 'contains'>('regex');
  let policyEffect = $state<'deny' | 'ask' | 'allow'>('deny');
  let policyReason = $state('');
  let policyPriority = $state(100);
  let policyGlobal = $state(true);

  // 技能导入
  let skillName = $state('');
  let skillDesc = $state('');
  let skillBody = $state('');

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

  const toggle = (rule: Rule) =>
    act(
      () => api(`/api/v1/rules/${rule.id}/enabled`, { method: 'PUT', body: { enabled: !rule.enabled } }),
      rule.enabled ? `已停用 ${rule.name}` : `已启用 ${rule.name}`
    ).catch(() => {});

  /** 软规则的报文。编辑时不带 name：任务按名字挂规则，改名等于全摘掉。 */
  const promptBody = () => ({
    kind: 'prompt',
    text: ruleText,
    global: ruleGlobal,
    priority: editing?.priority ?? 0
  });
  const policyBody = () => ({
    kind: 'policy',
    global: policyGlobal,
    priority: policyPriority,
    effect: policyEffect,
    reason: policyReason,
    match: policyPattern
      ? { tool: policyTool, arg: policyArg, any_of: [{ [policyKind]: policyPattern }] }
      : { tool: policyTool }
  });

  const savePromptRule = () =>
    act(
      async () => {
        if (editing) {
          await api(`/api/v1/rules/${editing.id}`, { method: 'PUT', body: promptBody() });
        } else {
          await api('/api/v1/rules', { method: 'POST', body: { ...promptBody(), name: ruleName } });
        }
        closeForm();
      },
      editing ? '软规则已更新，下一次执行开始生效' : '软规则已添加'
    );

  const savePolicy = () =>
    act(
      async () => {
        if (editing) {
          await api(`/api/v1/rules/${editing.id}`, { method: 'PUT', body: policyBody() });
        } else {
          await api('/api/v1/rules', { method: 'POST', body: { ...policyBody(), name: policyName } });
        }
        closeForm();
      },
      editing ? '策略已更新，立刻对新的工具调用生效' : '策略已添加，立刻对新的工具调用生效'
    );

  function closeForm() {
    adding = false;
    fieldErr = {};
    editing = null;
    ruleName = '';
    ruleText = '';
    ruleGlobal = false;
    policyName = '';
    policyPattern = '';
    policyReason = '';
    policyTool = 'Bash';
    policyArg = 'command';
    policyKind = 'regex';
    policyEffect = 'deny';
    policyPriority = 100;
    policyGlobal = true;
  }

  /** 把一条现有规则摊回表单。 */
  function openEdit(rule: Rule) {
    editing = rule;
    adding = true;
    if (rule.kind === 'prompt') {
      ruleName = rule.name;
      ruleText = promptText(rule);
      ruleGlobal = rule.scope === 'global';
      return;
    }
    const m = (rule.spec.match ?? {}) as Matcher;
    const first = m.any_of?.[0];
    const [kind, value] = first ? (Object.entries(first)[0] ?? ['regex', '']) : ['regex', ''];
    policyName = rule.name;
    policyTool = m.tool ?? '';
    policyArg = m.arg ?? 'command';
    policyKind = (['regex', 'glob', 'contains'].includes(kind) ? kind : 'regex') as typeof policyKind;
    policyPattern = value;
    policyEffect = String(rule.spec.effect ?? 'deny') as typeof policyEffect;
    policyReason = String(rule.spec.reason ?? '');
    policyPriority = rule.priority;
    policyGlobal = rule.scope === 'global';
  }

  let pendingDelete = $state<Rule | null>(null);
  const remove = (rule: Rule) =>
    act(() => api(`/api/v1/rules/${rule.id}`, { method: 'DELETE' }), `已删除 ${rule.name}`).catch(
      (e) => toastError(describeError(e))
    );

  const importSkill = () =>
    act(async () => {
      await api('/api/v1/skills', {
        method: 'POST',
        body: { name: skillName, description: skillDesc, body: skillBody }
      });
      skillName = '';
      skillDesc = '';
      skillBody = '';
      adding = false;
    }, '技能已导入').catch(() => {});

  /** 当前页签下的规则。软规则和硬策略语义不同，绝不能并成一张表。 */
  const kindRules = $derived(
    rules
      .filter((r) => r.kind === (tab === 'policy' ? 'policy' : 'prompt'))
      .sort((a, b) => b.priority - a.priority || a.name.localeCompare(b.name))
  );

  interface Matcher {
    tool?: string;
    arg?: string;
    any_of?: Array<Record<string, string>>;
  }
  /** 策略的匹配条件，压成一行给人扫。 */
  function matcherText(rule: Rule): string {
    const m = (rule.spec.match ?? {}) as Matcher;
    const pats = (m.any_of ?? []).map((p) => {
      const [kind, value] = Object.entries(p)[0] ?? ['', ''];
      return `${kind} ${value}`;
    });
    const head = m.tool ?? '任意工具';
    if (!pats.length) return `${head} 的所有调用`;
    return `${head}${m.arg ? `.${m.arg}` : ''} ~ ${pats.join(' | ')}`;
  }
  const promptText = (rule: Rule) => (rule.spec.text as string) ?? '';

  $effect(() => {
    void load();
  });
  $effect(() => {
    void tab;
    closeForm();
    error = null;
  });

  const EFFECT_TAG: Record<string, string> = { deny: 'danger', ask: 'warn', allow: 'ok' };
</script>

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
  title="删除{pendingDelete?.kind === 'policy' ? '策略' : '软规则'}「{pendingDelete?.name ?? ''}」？"
  danger
  confirmText="删除"
  {busy}
  onconfirm={() => {
    const rule = pendingDelete;
    pendingDelete = null;
    if (rule) void remove(rule);
  }}
>
  <p>还有任务挂着它的话会被拒绝，并列出是哪几个。</p>
  <p>
    删掉之后，历史执行记录里的规则指纹就对不上这条规则了；只是想暂时不生效的话，
    用「停用」更稳妥。
  </p>
</Confirm>

<PageHeader title="规则与策略">
  {#snippet sub()}
    <span>约束 AI 行为的两层。它们的强度<b>不一样</b>。</span>
  {/snippet}
  {#snippet actions()}
    {#if session.can('admin') && !adding}
      <button class="btn-primary" onclick={() => (adding = true)}>
        {tab === 'policy' ? '新增策略' : tab === 'prompt' ? '新增软规则' : '导入技能'}
      </button>
    {/if}
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <div class="callout danger">需要管理员权限：策略是这个系统的护栏本身。</div>
{:else}
  <div class="toolbar">
    <div class="seg">
      <button class:on={tab === 'policy'} onclick={() => (tab = 'policy')}>
        硬策略 <span class="count">{rules.filter((r) => r.kind === 'policy').length}</span>
      </button>
      <button class:on={tab === 'prompt'} onclick={() => (tab = 'prompt')}>
        软规则 <span class="count">{rules.filter((r) => r.kind === 'prompt').length}</span>
      </button>
      <button class:on={tab === 'skills'} onclick={() => (tab = 'skills')}>
        技能 <span class="count">{skills.length}</span>
      </button>
    </div>
  </div>

  {#if error}<div class="banner">{error}</div>{/if}

  {#if tab === 'policy'}
    <div class="callout">
      <strong>硬策略在工具调用边界强制执行，模型绕不过去。</strong>
      实测教训：只配一条 <code>rm -r</code> 的正则，模型会改用 <code>find -delete</code> 绕过去。
      <strong>要写成白名单形状</strong>——默认拒绝某个工具，再用更高优先级的规则放行具体的用法。
      <code>ask</code> 会把那次工具调用挂起等人点头，超时按拒绝处理。
    </div>

    <Modal
      open={adding}
      title={editing ? `编辑策略 ${editing.name}` : '新增策略'}
      size="lg"
      onclose={closeForm}
    >
      <div class="form-grid">
      <Field label="名称" hint={editing ? '名字不能改：任务是按名字挂规则的' : undefined} error={fieldErr.name}>
        {#snippet control(p)}
          <input {...p} bind:value={policyName} placeholder="prod-no-write" spellcheck="false" disabled={editing !== null} />
        {/snippet}
      </Field>
      <Field label="工具" error={fieldErr.match}>
        {#snippet control(p)}
          <input {...p} bind:value={policyTool} placeholder="Bash / remote_bash / remote_write" spellcheck="false" />
        {/snippet}
      </Field>
      <Field label="判决" error={fieldErr.effect}>
        {#snippet control(p)}
          <select {...p} bind:value={policyEffect}>
            <option value="deny">deny — 直接拒绝</option>
            <option value="ask">ask — 挂起等人审批</option>
            <option value="allow">allow — 放行（配合高优先级做白名单）</option>
          </select>
        {/snippet}
      </Field>
      <Field label="优先级" hint="数值大的先判，首个命中生效" error={fieldErr.priority}>
        {#snippet control(p)}
          <input {...p} type="number" bind:value={policyPriority} />
        {/snippet}
      </Field>
      <Field label="作用范围" error={fieldErr.global}>
        {#snippet control(p)}
          <select {...p} bind:value={policyGlobal}>
            <option value={true}>全局 — 所有任务都生效</option>
            <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
          </select>
        {/snippet}
      </Field>
      <Field label="匹配参数" error={fieldErr.arg}>
        {#snippet control(p)}
          <input {...p} bind:value={policyArg} placeholder="command / path" spellcheck="false" />
        {/snippet}
      </Field>
      <Field label="匹配方式" error={fieldErr.kind}>
        {#snippet control(p)}
          <select {...p} bind:value={policyKind}>
            <option value="regex">正则</option>
            <option value="glob">glob</option>
            <option value="contains">包含</option>
          </select>
        {/snippet}
      </Field>
      <Field label="模式" hint="留空表示匹配这个工具的所有调用" error={fieldErr.pattern} wide>
        {#snippet control(p)}
          <input {...p} bind:value={policyPattern} placeholder={'^\\s*rm\\s+-rf\\s+/'} spellcheck="false" class="mono" />
        {/snippet}
      </Field>
      <Field label="原因" hint="会作为 tool_result 回给模型，写清为什么比写「不行」有用" error={fieldErr.reason} wide>
        {#snippet control(p)}
          <input {...p} bind:value={policyReason} placeholder="生产机禁止递归删除" />
        {/snippet}
      </Field>
      </div>
      {#snippet footer()}
        <button class="btn-ghost" onclick={closeForm} disabled={busy}>取消</button>
        <button class="btn-primary" onclick={savePolicy} disabled={busy || !policyName || !policyReason}>
          {editing ? '保存' : '添加策略'}
        </button>
      {/snippet}
    </Modal>

    {#if !loaded}
      <div class="card"><Loading rows={3} /></div>
    {:else if kindRules.length}
      <div class="card flush">
        <table>
          <thead>
            <tr><th>名称</th><th>判决</th><th>匹配</th><th>原因</th><th>范围</th><th>优先级</th><th class="act"></th></tr>
          </thead>
          <tbody>
            {#each kindRules as rule (rule.id)}
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
                    <button class="btn-ghost btn-sm" disabled={busy} onclick={() => toggle(rule)}>
                      {rule.enabled ? '停用' : '启用'}
                    </button>
                    <button class="btn-ghost btn-sm btn-icon" title="编辑" aria-label="编辑" disabled={busy} onclick={() => openEdit(rule)}>
                      <Icon name="pencil" />
                    </button>
                    <!-- 只是暂时不生效的话用停用：删除会让历史 run 的规则指纹对不上 -->
                    <button
                      class="btn-ghost btn-sm btn-icon danger"
                      title="删除"
                      aria-label="删除规则"
                      disabled={busy}
                      onclick={() => (pendingDelete = rule)}
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
          <button class="btn-primary" onclick={() => (adding = true)}>新增策略</button>
        {/snippet}
      </Empty>
    {/if}
  {:else if tab === 'prompt'}
    <div class="callout">
      <strong>软规则注入 system prompt，只影响模型的倾向。</strong>
      模型<strong>可以不听</strong>。工具结果是不可信输入，能影响模型对 prompt 的遵守——所以
      真正要拦住的事情必须同时配一条硬策略。软规则的价值在于让模型少走弯路，不在于阻止它。
    </div>

    <Modal
      open={adding}
      title={editing ? `编辑软规则 ${editing.name}` : '新增软规则'}
      onclose={closeForm}
    >
      <div class="form-grid">
        <Field
          label="名称"
          hint={editing ? '名字不能改：任务是按名字挂规则的' : undefined}
          error={fieldErr.name}
        >
          {#snippet control(p)}
            <input {...p} bind:value={ruleName} placeholder="no-restart" spellcheck="false" disabled={editing !== null} />
          {/snippet}
        </Field>
        <Field label="作用范围" error={fieldErr.global}>
          {#snippet control(p)}
            <select {...p} bind:value={ruleGlobal}>
              <option value={true}>全局 — 所有任务都生效</option>
              <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
            </select>
          {/snippet}
        </Field>
        <Field label="规则文本" error={fieldErr.text} wide>
          {#snippet control(p)}
            <textarea {...p} bind:value={ruleText} rows="4" class="prose" placeholder="不要重启任何服务。需要重启时先报告，等人确认。"></textarea>
          {/snippet}
        </Field>
      </div>
      {#snippet footer()}
        <button class="btn-ghost" onclick={closeForm} disabled={busy}>取消</button>
        <button class="btn-primary" onclick={savePromptRule} disabled={busy || !ruleName || !ruleText}>
          {editing ? '保存' : '添加规则'}
        </button>
      {/snippet}
    </Modal>

    {#if !loaded}
      <div class="card"><Loading rows={3} /></div>
    {:else if kindRules.length}
      <div class="card flush">
        <table>
          <thead>
            <tr><th>名称</th><th>内容</th><th>范围</th><th class="act"></th></tr>
          </thead>
          <tbody>
            {#each kindRules as rule (rule.id)}
              <tr class:off={!rule.enabled} class:on={editing?.id === rule.id}>
                <td class="mono name">{rule.name}</td>
                <td class="muted text">{promptText(rule)}</td>
                <td>
                  <span class="tag" class:accent={rule.scope === 'global'}>
                    {rule.scope === 'global' ? '全局' : '按任务挂载'}
                  </span>
                </td>
                <td class="act">
                  <div class="row">
                    <button class="btn-ghost btn-sm" disabled={busy} onclick={() => toggle(rule)}>
                      {rule.enabled ? '停用' : '启用'}
                    </button>
                    <button class="btn-ghost btn-sm btn-icon" title="编辑" aria-label="编辑" disabled={busy} onclick={() => openEdit(rule)}>
                      <Icon name="pencil" />
                    </button>
                    <!-- 只是暂时不生效的话用停用：删除会让历史 run 的规则指纹对不上 -->
                    <button
                      class="btn-ghost btn-sm btn-icon danger"
                      title="删除"
                      aria-label="删除规则"
                      disabled={busy}
                      onclick={() => (pendingDelete = rule)}
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
      <Empty title="还没有软规则" hint="写一句给模型的话，比如「不要重启任何服务」。它会进每次执行的 system prompt。">
        {#snippet action()}
          <button class="btn-primary" onclick={() => (adding = true)}>新增软规则</button>
        {/snippet}
      </Empty>
    {/if}
  {:else}
    <div class="callout">
      <strong>技能是开箱即用的上下文包。</strong>
      勾选后会落到 run 工作目录的 <code>.claude/skills/</code>，渐进式披露由 CLI 自己完成
      ——只有 name 和 description 进上下文，正文按需加载。在任务的 AI 步骤里勾选即可。
    </div>

    <Modal open={adding} title="导入技能" size="lg" onclose={closeForm}>
      <div class="form-grid">
        <Field label="名称" error={fieldErr.name}>
          {#snippet control(p)}
            <input {...p} bind:value={skillName} placeholder="linux-perf" spellcheck="false" />
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
            <input {...p} bind:value={skillDesc} placeholder="Linux 性能排查：CPU / 内存 / IO / 网络的定位顺序" />
          {/snippet}
        </Field>
        <Field label="正文（Markdown，就是 SKILL.md 的内容）" error={fieldErr.body} wide>
          {#snippet control(p)}
            <textarea {...p} bind:value={skillBody} rows="12" spellcheck="false"
              placeholder="## 定位顺序&#10;&#10;1. 先看整机负载：uptime、vmstat 1&#10;2. ..."></textarea>
          {/snippet}
        </Field>
      </div>
      {#snippet footer()}
        <button class="btn-ghost" onclick={closeForm} disabled={busy}>取消</button>
        <button class="btn-primary" onclick={importSkill} disabled={busy || !skillName || !skillDesc || !skillBody}>
          导入
        </button>
      {/snippet}
    </Modal>

    {#if !loaded}
      <div class="card"><Loading rows={3} /></div>
    {:else if skills.length}
      <div class="card flush">
        <table>
          <thead><tr><th>名称</th><th>描述</th><th>版本</th><th>内容哈希</th></tr></thead>
          <tbody>
            {#each skills as s (s.name)}
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
          <button class="btn-primary" onclick={() => (adding = true)}>导入技能</button>
        {/snippet}
      </Empty>
    {/if}
  {/if}
{/if}

<style>
  .callout {
    margin-bottom: var(--s4);
  }
  .name {
    font-weight: 500;
    color: var(--fg);
    white-space: nowrap;
  }
  .reason,
  .text {
    max-width: 40ch;
    overflow-wrap: anywhere;
  }
  td.small {
    max-width: 32ch;
    overflow-wrap: anywhere;
  }
  textarea.prose {
    font-family: var(--font);
    font-size: var(--t-base);
  }
</style>
