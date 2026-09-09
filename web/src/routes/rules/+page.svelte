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
  import { api, ApiFailure } from '$api/client';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';

  interface Skill {
    name: string;
    version: string;
    description: string;
    content_hash: string;
  }

  interface Rule {
    id: string;
    name: string;
    kind: 'prompt' | 'policy';
    scope: 'global' | 'task';
    spec: Record<string, unknown>;
    priority: number;
    enabled: boolean;
  }

  let rules = $state<Rule[]>([]);
  let skills = $state<Skill[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let tab = $state<'policy' | 'prompt' | 'skills'>('policy');

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
      const [s, r] = await Promise.all([
        api<{ items: Skill[] }>('/api/v1/skills'),
        api<{ items: Rule[] }>('/api/v1/rules')
      ]);
      skills = s.items;
      rules = r.items;
    } catch (e) {
      error = describe(e);
    }
  }

  const ruleText_ = (rule: Rule) =>
    typeof rule.spec === 'string' ? rule.spec : ((rule.spec.text as string) ?? '');

  const toggle = (rule: Rule) =>
    act(() =>
      api(`/api/v1/rules/${rule.id}/enabled`, {
        method: 'PUT',
        body: { enabled: !rule.enabled }
      })
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
    });

  function describe(e: unknown): string {
    if (e instanceof ApiFailure) {
      const details = (e.body as { details?: Array<{ message: string }> }).details;
      return details?.length ? details.map((d) => d.message).join('；') : e.message;
    }
    return String(e);
  }

  async function act(run: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await run();
      await load();
    } catch (e) {
      error = describe(e);
    } finally {
      busy = false;
    }
  }

  const createPromptRule = () =>
    act(async () => {
      await api('/api/v1/rules', {
        method: 'POST',
        body: { kind: 'prompt', name: ruleName, text: ruleText, global: ruleGlobal, priority: 0 }
      });
      ruleName = '';
      ruleText = '';
    });

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
    });

  /** 当前页签下的规则。软规则和硬策略语义不同，绝不能并成一张表。 */
  const kindRules = $derived(rules.filter((r) => r.kind === (tab === 'policy' ? 'policy' : 'prompt')));

  $effect(() => {
    void load();
  });
</script>

<PageHeader title="规则与策略">
  {#snippet sub()}
    <span>约束 AI 行为的两层。它们的强度**不一样**。</span>
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <p class="bad">需要管理员权限：策略是这个系统的护栏本身。</p>
{:else}
  <div class="tabs">
    <button class:on={tab === 'policy'} onclick={() => (tab = 'policy')}>硬策略</button>
    <button class:on={tab === 'prompt'} onclick={() => (tab = 'prompt')}>软规则</button>
    <button class:on={tab === 'skills'} onclick={() => (tab = 'skills')}>技能</button>
  </div>

  {#if error}<p class="bad">{error}</p>{/if}

  {#if tab === 'policy'}
    <div class="card note">
      <strong>硬策略在工具调用边界强制执行，模型绕不过去。</strong>
      <p>
        实测教训：只配一条 <code>rm -r</code> 的正则，模型会改用 <code>find -delete</code> 绕过去。
        <b>要写成白名单形状</b>——默认拒绝某个工具，再用更高优先级的规则放行具体的用法。
        黑名单对一个会换说法的对手是无效的。
      </p>
      <p class="faint">
        <code>ask</code> 会把那次工具调用挂起等人点头，超时按拒绝处理。
      </p>
    </div>

    <div class="card form">
      <h2>新增策略</h2>
      <div class="grid">
        <label class="field">名称<input bind:value={policyName} placeholder="prod-no-write" /></label>
        <label class="field">
          工具
          <input bind:value={policyTool} placeholder="Bash / remote_bash / remote_write" />
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
        </label>
        <label class="field">
          作用范围
          <select bind:value={policyGlobal}>
            <option value={true}>全局 — 所有任务都生效</option>
            <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
          </select>
        </label>
      </div>
      <div class="grid">
        <label class="field">匹配参数<input bind:value={policyArg} placeholder="command / path" /></label>
        <label class="field">
          匹配方式
          <select bind:value={policyKind}>
            <option value="regex">正则</option>
            <option value="glob">glob</option>
            <option value="contains">包含</option>
          </select>
        </label>
        <label class="field wide">
          模式（留空表示匹配这个工具的所有调用）
          <input bind:value={policyPattern} placeholder="^\s*rm\s+-rf\s+/" />
        </label>
      </div>
      <label class="field">
        原因（会作为 tool_result 回给模型，写清为什么比写「不行」有用）
        <input bind:value={policyReason} placeholder="生产机禁止递归删除" />
      </label>
      <button class="btn-primary" onclick={createPolicy} disabled={busy || !policyName || !policyReason}>
        添加策略
      </button>
    </div>

    {#if kindRules.length}
      <h2 class="list-head">已有的策略</h2>
      <table>
        <thead>
          <tr><th>名称</th><th>内容</th><th>范围</th><th>优先级</th><th></th></tr>
        </thead>
        <tbody>
          {#each kindRules as rule (rule.id)}
            <tr class:off={!rule.enabled}>
              <td class="mono">{rule.name}</td>
              <td class="muted">
                {#if 'effect' in rule.spec}
                  {rule.spec.effect} · {rule.spec.reason}
                {:else}
                  {ruleText_(rule)}
                {/if}
              </td>
              <td>
                <span class="tag" class:accent={rule.scope === 'global'}>
                  {rule.scope === 'global' ? '全局' : '按任务挂载'}
                </span>
              </td>
              <td class="faint">{rule.priority}</td>
              <td class="act">
                <!-- 只有停用没有删除：规则文本进过 runs.rules_hash，
                     删掉之后历史 run 就解释不了了 -->
                <button class="btn-sm" disabled={busy} onclick={() => toggle(rule)}>
                  {rule.enabled ? '停用' : '启用'}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

  {:else if tab === 'prompt'}
    <div class="card note">
      <strong>软规则注入 system prompt，只影响模型的倾向。</strong>
      <p>
        模型**可以不听**。工具结果是不可信输入，能影响模型对 prompt 的遵守——所以
        真正要拦住的事情必须同时配一条硬策略。软规则的价值在于让模型少走弯路，
        不在于阻止它。
      </p>
    </div>

    <div class="card form">
      <h2>新增软规则</h2>
      <label class="field">名称<input bind:value={ruleName} placeholder="no-restart" /></label>
      <label class="field">
        规则文本
        <textarea bind:value={ruleText} rows="3" placeholder="不要重启任何服务。需要重启时先报告，等人确认。"></textarea>
      </label>
        <label class="field">
          作用范围
          <select bind:value={ruleGlobal}>
            <option value={true}>全局 — 所有任务都生效</option>
            <option value={false}>按任务挂载 — 只对勾上它的任务生效</option>
          </select>
        </label>
      <button class="btn-primary" onclick={createPromptRule} disabled={busy || !ruleName || !ruleText}>
        添加规则
      </button>
    </div>

    {#if kindRules.length}
      <h2 class="list-head">已有的软规则</h2>
      <table>
        <thead>
          <tr><th>名称</th><th>内容</th><th>范围</th><th>优先级</th><th></th></tr>
        </thead>
        <tbody>
          {#each kindRules as rule (rule.id)}
            <tr class:off={!rule.enabled}>
              <td class="mono">{rule.name}</td>
              <td class="muted">
                {#if 'effect' in rule.spec}
                  {rule.spec.effect} · {rule.spec.reason}
                {:else}
                  {ruleText_(rule)}
                {/if}
              </td>
              <td>
                <span class="tag" class:accent={rule.scope === 'global'}>
                  {rule.scope === 'global' ? '全局' : '按任务挂载'}
                </span>
              </td>
              <td class="faint">{rule.priority}</td>
              <td class="act">
                <!-- 只有停用没有删除：规则文本进过 runs.rules_hash，
                     删掉之后历史 run 就解释不了了 -->
                <button class="btn-sm" disabled={busy} onclick={() => toggle(rule)}>
                  {rule.enabled ? '停用' : '启用'}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

  {:else}
    <div class="card note">
      <strong>技能是开箱即用的上下文包。</strong>
      <p>
        勾选后会落到 run 工作目录的 <code>.claude/skills/</code>，渐进式披露由 CLI 自己完成
        ——只有 name 和 description 进上下文，正文按需加载。在 AI 节点的
        <code>skills</code> 数组里写名字即可。
      </p>
    </div>

    {#if skills.length}
      <table>
        <thead><tr><th>名称</th><th>描述</th><th>版本</th><th>内容哈希</th></tr></thead>
        <tbody>
          {#each skills as s (s.name)}
            <tr>
              <td class="mono">{s.name}</td>
              <td class="muted">{s.description}</td>
              <td class="faint">{s.version}</td>
              <td class="mono faint">{s.content_hash.slice(0, 12)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else}
      <Empty
        title="还没有技能包"
        hint="技能是一份写给 AI 的操作手册：这类问题怎么查、公司内部的命令怎么敲。导入之后在任务的 AI 步骤里勾选。"
      />
    {/if}

    <div class="card form">
      <h2>导入技能</h2>
      <div class="grid">
        <label class="field">
          名称
          <input bind:value={skillName} placeholder="linux-perf" />
        </label>
        <label class="field wide">
          描述
          <!-- 渐进式披露时模型只看得到描述。写不清楚等于这个技能不会被用上 -->
          <input bind:value={skillDesc} placeholder="Linux 性能排查：CPU / 内存 / IO / 网络的定位顺序" />
        </label>
      </div>
      <label class="field">
        正文（Markdown，就是 SKILL.md 的内容）
        <textarea bind:value={skillBody} rows="10" spellcheck="false"
          placeholder="## 定位顺序&#10;&#10;1. 先看整机负载：uptime、vmstat 1&#10;2. ..."></textarea>
      </label>
      <button
        class="btn-primary"
        onclick={importSkill}
        disabled={busy || !skillName || !skillDesc || !skillBody}
      >
        导入
      </button>
    </div>
  {/if}
{/if}

<style>
  .list-head {
    margin: var(--s5) 0 var(--s2);
  }
  tr.off {
    opacity: 0.45;
  }
  td.act {
    width: 1%;
    text-align: right;
  }
  .tabs {
    display: flex;
    gap: 2px;
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r2);
    padding: 2px;
    margin-bottom: var(--s4);
    width: fit-content;
  }
  .tabs button {
    border: none;
    background: transparent;
    color: var(--fg-dim);
  }
  .tabs button.on {
    background: var(--surface-3);
    color: var(--fg);
  }
  .note {
    margin-bottom: var(--s3);
    border-color: var(--accent-dim);
  }
  .note strong {
    display: block;
    margin-bottom: var(--s2);
  }
  .note p {
    margin: 0 0 var(--s2);
    font-size: 0.85rem;
    color: var(--fg-dim);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: var(--s3);
  }
  .wide {
    grid-column: 1 / -1;
  }
  .form button {
    align-self: flex-start;
  }
</style>
