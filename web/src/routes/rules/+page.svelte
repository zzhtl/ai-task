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
  import { api, describeError } from '$api/client';
  import { listRules, listSkills, type Rule, type Skill } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';

  type Tab = 'policy' | 'prompt' | 'skills';

  let rules = $state<Rule[]>([]);
  let skills = $state<Skill[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let busy = $state(false);
  let tab = $state<Tab>('policy');
  let adding = $state(false);

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
    try {
      await run();
      await load();
      if (done) toast(done);
    } catch (e) {
      error = describeError(e);
    } finally {
      busy = false;
    }
  }

  const toggle = (rule: Rule) =>
    act(
      () => api(`/api/v1/rules/${rule.id}/enabled`, { method: 'PUT', body: { enabled: !rule.enabled } }),
      rule.enabled ? `已停用 ${rule.name}` : `已启用 ${rule.name}`
    ).catch((e) => toastError(describeError(e)));

  const createPromptRule = () =>
    act(async () => {
      await api('/api/v1/rules', {
        method: 'POST',
        body: { kind: 'prompt', name: ruleName, text: ruleText, global: ruleGlobal, priority: 0 }
      });
      ruleName = '';
      ruleText = '';
      adding = false;
    }, '软规则已添加');

  const createPolicy = () =>
    act(async () => {
      await api('/api/v1/rules', {
        method: 'POST',
        body: {
          kind: 'policy',
          name: policyName,
          global: policyGlobal,
          priority: policyPriority,
          effect: policyEffect,
          reason: policyReason,
          match: policyPattern
            ? { tool: policyTool, arg: policyArg, any_of: [{ [policyKind]: policyPattern }] }
            : { tool: policyTool }
        }
      });
      policyName = '';
      policyPattern = '';
      policyReason = '';
      adding = false;
    }, '策略已添加，立刻对新的工具调用生效');

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
    }, '技能已导入');

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
    adding = false;
    error = null;
  });

  const EFFECT_TAG: Record<string, string> = { deny: 'danger', ask: 'warn', allow: 'ok' };
</script>

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

    {#if adding}
      <section class="card form">
        <header class="card-head">
          <h2>新增策略</h2>
          <span class="spacer"></span>
          <button class="btn-ghost btn-sm" onclick={() => (adding = false)}>收起</button>
        </header>
        <div class="form-grid">
          <label class="field">名称<input bind:value={policyName} placeholder="prod-no-write" spellcheck="false" /></label>
          <label class="field">
            工具
            <input bind:value={policyTool} placeholder="Bash / remote_bash / remote_write" spellcheck="false" />
          </label>
          <label class="field">
            判决
            <select bind:value={policyEffect}>
              <option value="deny">deny — 直接拒绝</option>
              <option value="ask">ask — 挂起等人审批</option>
              <option value="allow">allow — 放行（配合高优先级做白名单）</option>
            </select>
          </label>
          <label class="field">
            优先级
            <input type="number" bind:value={policyPriority} />
            <span class="hint">数值大的先判，首个命中生效</span>
          </label>
          <label class="field">
            作用范围
            <select bind:value={policyGlobal}>
              <option value={true}>全局 — 所有任务都生效</option>
              <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
            </select>
          </label>
        </div>
        <div class="form-grid">
          <label class="field">匹配参数<input bind:value={policyArg} placeholder="command / path" spellcheck="false" /></label>
          <label class="field">
            匹配方式
            <select bind:value={policyKind}>
              <option value="regex">正则</option>
              <option value="glob">glob</option>
              <option value="contains">包含</option>
            </select>
          </label>
          <label class="field wide">
            模式
            <input bind:value={policyPattern} placeholder={'^\\s*rm\\s+-rf\\s+/'} spellcheck="false" class="mono" />
            <span class="hint">留空表示匹配这个工具的所有调用</span>
          </label>
        </div>
        <label class="field">
          原因
          <input bind:value={policyReason} placeholder="生产机禁止递归删除" />
          <span class="hint">会作为 tool_result 回给模型，写清为什么比写「不行」有用</span>
        </label>
        <div class="form-actions">
          <button class="btn-primary" onclick={createPolicy} disabled={busy || !policyName || !policyReason}>
            添加策略
          </button>
        </div>
      </section>
    {/if}

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
              <tr class:off={!rule.enabled}>
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
                  <!-- 只有停用没有删除：规则文本进过 runs.rules_hash，
                       删掉之后历史 run 就解释不了了 -->
                  <button class="btn-ghost btn-sm" disabled={busy} onclick={() => toggle(rule)}>
                    {rule.enabled ? '停用' : '启用'}
                  </button>
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

    {#if adding}
      <section class="card form">
        <header class="card-head">
          <h2>新增软规则</h2>
          <span class="spacer"></span>
          <button class="btn-ghost btn-sm" onclick={() => (adding = false)}>收起</button>
        </header>
        <div class="form-grid">
          <label class="field">名称<input bind:value={ruleName} placeholder="no-restart" spellcheck="false" /></label>
          <label class="field">
            作用范围
            <select bind:value={ruleGlobal}>
              <option value={true}>全局 — 所有任务都生效</option>
              <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
            </select>
          </label>
        </div>
        <label class="field">
          规则文本
          <textarea bind:value={ruleText} rows="3" class="prose" placeholder="不要重启任何服务。需要重启时先报告，等人确认。"></textarea>
        </label>
        <div class="form-actions">
          <button class="btn-primary" onclick={createPromptRule} disabled={busy || !ruleName || !ruleText}>
            添加规则
          </button>
        </div>
      </section>
    {/if}

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
              <tr class:off={!rule.enabled}>
                <td class="mono name">{rule.name}</td>
                <td class="muted text">{promptText(rule)}</td>
                <td>
                  <span class="tag" class:accent={rule.scope === 'global'}>
                    {rule.scope === 'global' ? '全局' : '按任务挂载'}
                  </span>
                </td>
                <td class="act">
                  <button class="btn-ghost btn-sm" disabled={busy} onclick={() => toggle(rule)}>
                    {rule.enabled ? '停用' : '启用'}
                  </button>
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

    {#if adding}
      <section class="card form">
        <header class="card-head">
          <h2>导入技能</h2>
          <span class="spacer"></span>
          <button class="btn-ghost btn-sm" onclick={() => (adding = false)}>收起</button>
        </header>
        <div class="form-grid">
          <label class="field">名称<input bind:value={skillName} placeholder="linux-perf" spellcheck="false" /></label>
          <label class="field wide">
            描述
            <!-- 渐进式披露时模型只看得到描述。写不清楚等于这个技能不会被用上 -->
            <input bind:value={skillDesc} placeholder="Linux 性能排查：CPU / 内存 / IO / 网络的定位顺序" />
            <span class="hint">模型只凭这一句决定要不要加载它，写清楚适用场景</span>
          </label>
        </div>
        <label class="field">
          正文（Markdown，就是 SKILL.md 的内容）
          <textarea bind:value={skillBody} rows="10" spellcheck="false"
            placeholder="## 定位顺序&#10;&#10;1. 先看整机负载：uptime、vmstat 1&#10;2. ..."></textarea>
        </label>
        <div class="form-actions">
          <button class="btn-primary" onclick={importSkill} disabled={busy || !skillName || !skillDesc || !skillBody}>
            导入
          </button>
        </div>
      </section>
    {/if}

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
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
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
    font-size: 0.86rem;
  }
</style>
