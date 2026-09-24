<script lang="ts">
  /**
   * 有序步骤编辑器。
   *
   * 每一步说清楚：做什么、在哪台机器上、谁来做。前后自动串起来，
   * **每步都能看见上一步的结果**——不串的话「按顺序」就没有意义。
   */
  import { newStep, reachOf, REACH_TOOLS, type Composition, type Reach, type Step } from './compose';
  import Icon from '$lib/ui/Icon.svelte';
  import StepRail from '$lib/ui/StepRail.svelte';
  // 这三个类型在 models.ts 里有唯一定义。之前这里各抄了一份：
  // 后端加字段时抄出去的那份不会报错，只会悄悄对不上。
  import type { AiCli, Host, Skill } from '$api/models';


  let {
    comp = $bindable(),
    hosts,
    skills
  }: { comp: Composition; hosts: Host[]; skills: Skill[] } = $props();

  const MODELS = [
    { id: 'claude-sonnet-5', label: 'Sonnet 5 — 默认，日常够用' },
    { id: 'claude-opus-5', label: 'Opus 5 — 最强最贵' },
    { id: 'claude-haiku-4-5-20251001', label: 'Haiku 4.5 — 最快最便宜' }
  ];

  /**
   * 老任务可能没指定模型。**不静默替换掉**——那会在用户没察觉的情况下
   * 改掉一个正在跑的任务的行为。多给一个选项，把现状如实摆出来。
   */
  const modelsFor = (step: Step) =>
    step.model === null || MODELS.some((m) => m.id === step.model)
      ? step.model === null
        ? [{ id: '', label: '未指定 — 由 CLI 决定，建议改掉' }, ...MODELS]
        : MODELS
      : [{ id: step.model, label: `${step.model}（任务里原本指定的）` }, ...MODELS];

  const REACHES: Array<{ id: Reach; label: string; hint: string }> = [
    { id: 'read_only', label: '只读', hint: '只能看文件（Read/Glob/Grep）。跑不了命令。' },
    { id: 'run_commands', label: '可执行命令', hint: '加上 Bash。能跑 df / systemctl 这些。' },
    { id: 'edit_files', label: '可改文件', hint: '再加上 Write/Edit。能落地修改。' }
  ];

  const KINDS: Array<{ id: Step['kind']; label: string; hint: string }> = [
    { id: 'ai', label: 'AI 执行', hint: '把一段话交给 AI，它自己决定怎么做' },
    { id: 'shell', label: '运行命令', hint: '确定性的一条命令，不经过模型' },
    { id: 'approval', label: '人工确认', hint: '停下来等人点头。超时按拒绝处理' }
  ];

  const clisOf = (hostId: string | null) =>
    hostId ? (hosts.find((h) => h.id === hostId)?.ai_clis ?? []) : [];

  function add(kind: Step['kind']) {
    const step = newStep(kind);
    // 默认接着上一步。大多数流程是线性的，每加一步都要手动勾一次太烦
    const prev = comp.steps.at(-1);
    if (prev) step.sees = [prev.uid];
    comp.steps = [...comp.steps, step];
  }

  /** 这一步之前的所有步骤。只能看更早的——看后面的就是循环依赖。 */
  const earlier = (i: number) => comp.steps.slice(0, i);

  /** 勾着、但已经不在这一步前面的那些步骤的标题。 */
  const stale = (step: Step, i: number) =>
    step.sees
      .map((uid) => comp.steps.findIndex((s) => s.uid === uid))
      .filter((at) => at >= i)
      .map((at) => comp.steps[at].title || `第 ${at + 1} 步`);

  function toggleSees(step: Step, uid: string) {
    step.sees = step.sees.includes(uid)
      ? step.sees.filter((u) => u !== uid)
      : [...step.sees, uid];
  }
  function remove(i: number) {
    comp.steps = comp.steps.filter((_, at) => at !== i);
  }
  function move(i: number, delta: number) {
    const to = i + delta;
    if (to < 0 || to >= comp.steps.length) return;
    const next = [...comp.steps];
    [next[i], next[to]] = [next[to], next[i]];
    comp.steps = next;
  }

  /** 换机器之后，原来选的 CLI 可能在新机器上不存在，退回中心执行。 */
  function onHostChange(step: Step) {
    if (step.runner.kind !== 'host_cli') return;
    const wanted = step.runner.cli;
    // 新机器上没有原来那个 CLI 就退回中心执行，别留一个必然失败的配置
    if (!clisOf(step.hostId).some((c) => c.name === wanted)) {
      step.runner = { kind: 'center' };
    }
  }

  const placeholder = (kind: Step['kind']) =>
    kind === 'ai'
      ? '用自然语言说清楚要做什么。比如：读 /var/log/app.log，统计最近一天的 ERROR 行数，判断是否异常。只读，不要改任何文件。'
      : kind === 'shell'
        ? 'systemctl restart nginx'
        : '';
</script>

<div class="steps">
  {#each comp.steps as step, i (step.uid)}
    <article class="step {step.kind}">
      <StepRail index={i + 1} last={i === comp.steps.length - 1} kind={step.kind} />

      <div class="body">
        <header>
          <div class="seg">
            {#each KINDS as k (k.id)}
              <button class:on={step.kind === k.id} title={k.hint} onclick={() => (step.kind = k.id)}>
                {k.label}
              </button>
            {/each}
          </div>
          <span class="spacer"></span>
          <button class="btn-ghost btn-sm btn-icon" disabled={i === 0} onclick={() => move(i, -1)} title="上移" aria-label="上移">
            <Icon name="arrow-up" />
          </button>
          <button
            class="btn-ghost btn-sm btn-icon"
            disabled={i === comp.steps.length - 1}
            onclick={() => move(i, 1)}
            title="下移"
            aria-label="下移"
          >
            <Icon name="arrow-down" />
          </button>
          <button
            class="btn-ghost btn-sm btn-icon danger"
            disabled={comp.steps.length === 1}
            onclick={() => remove(i)}
            title="删除这一步"
            aria-label="删除这一步"
          >
            <Icon name="close" />
          </button>
        </header>

        <input class="title" bind:value={step.title} placeholder="这一步叫什么" />

        {#if step.kind !== 'approval'}
          <textarea
            bind:value={step.body}
            rows={step.kind === 'ai' ? 4 : 2}
            placeholder={placeholder(step.kind)}
            spellcheck="false"
            class:mono={step.kind === 'shell'}
          ></textarea>
        {:else}
          <p class="hint">
            执行到这里会停下来等人在<a href="/approvals">审批页</a>点头。
            超时按<b>拒绝</b>处理——审批门的意义就在于「没人点头就不做」。
          </p>
        {/if}

        {#if i > 0}
          <!-- 只串相邻两步不够：最后一步回写 Jira 要同时用到第 1 步的单号和
               第 3 步的修复结果。勾中的会以小标题的形式出现在提示词里。 -->
          <div class="sees">
            <span class="hint">能看到哪几步的结果</span>
            <div class="chips">
              {#each earlier(i) as prior, at (prior.uid)}
                <button
                  class="btn-sm chip"
                  class:on={step.sees.includes(prior.uid)}
                  title={prior.title || `第 ${at + 1} 步`}
                  onclick={() => toggleSees(step, prior.uid)}
                >
                  {at + 1} · {prior.title || '未命名'}
                </button>
              {/each}
            </div>
            {#if step.sees.length === 0}
              <span class="hint warn-text">一步都不看的话，它跟前面几步就是互不相干的任务。</span>
            {/if}
            <!-- 把一步挪到它的数据来源前面，那条引用就不成立了。
                 静默丢掉的话，人不会发现这一步突然看不见东西了 -->
            {#each stale(step, i) as gone (gone)}
              <span class="hint warn-text">
                「{gone}」现在排在这一步后面，看不到了。挪回去，或者取消勾选。
              </span>
            {/each}
          </div>
        {/if}

        <div class="opts">
          <label class="field">
            在哪台机器
            <select bind:value={step.hostId} onchange={() => onHostChange(step)}>
              <option value={null}>本机</option>
              {#each hosts as h (h.id)}<option value={h.id}>{h.name}</option>{/each}
            </select>
          </label>

          {#if step.kind === 'ai'}
            <label class="field">
              谁来执行
              <select
                value={step.runner.kind === 'host_cli' ? step.runner.cli : 'center'}
                onchange={(e) => {
                  const v = e.currentTarget.value;
                  step.runner = v === 'center' ? { kind: 'center' } : { kind: 'host_cli', cli: v };
                }}
              >
                <option value="center">中心的 Claude Code</option>
                {#each clisOf(step.hostId) as cli (cli.name)}
                  <option value={cli.name}>目标机上的 {cli.name}</option>
                {/each}
              </select>
            </label>

            <label class="field">
              能动到什么
              <!-- 选中的档位是从真实工具清单**推**出来的，不是存下来的。
                   反过来（存档位、按档位生成清单）会把 ["Read"] 这种更紧的
                   白名单在保存时悄悄放宽成三个工具。 -->
              <select
                value={reachOf(step.tools)}
                onchange={(e) => (step.tools = [...REACH_TOOLS[e.currentTarget.value as Reach]])}
              >
                {#each REACHES as r (r.id)}<option value={r.id} title={r.hint}>{r.label}</option>{/each}
              </select>
            </label>

            <label class="field">
              模型
              <select bind:value={step.model}>
                {#each modelsFor(step) as m (m.id)}
                  <option value={m.id || null}>{m.label}</option>
                {/each}
              </select>
            </label>
          {/if}

          <label class="field narrow">
            超时（秒）
            <input type="number" bind:value={step.timeoutS} min="30" max="86400" placeholder="默认" />
          </label>
        </div>

        {#if step.kind === 'ai'}
          <p class="hint">
            <!-- 清单和档位对不上时，以清单为准把它列出来：那才是真正生效的东西 -->
            {#if step.tools.join() !== REACH_TOOLS[reachOf(step.tools)].join()}
              实际工具：<code>{step.tools.join('、') || 'CLI 默认'}</code>。改上面的档位会整体替换掉它。
            {:else}
              {REACHES.find((r) => r.id === reachOf(step.tools))?.hint}
            {/if}
            {#if reachOf(step.tools) !== 'read_only'}
              真正要拦住的用法去<a href="/rules">规则页</a>配硬策略——那是在工具调用边界强制的。
            {/if}
          </p>
        {/if}

        {#if step.kind === 'ai' && step.runner.kind === 'host_cli'}
          <p class="hint warn-text">
            AI 直接在那台机器上跑，文件就在本地。但<b>它的内置工具不经过策略层</b>
            ——Bash/Write 会直接落地，中心只看得见输出。
          </p>
        {/if}

        {#if step.kind === 'ai' && !skills.length}
          <p class="hint">
            还没有导入过技能。技能是一份写给 AI 的操作手册（怎么查这类问题、
            公司内部的命令怎么敲），在<a href="/rules">规则页</a>导入后可以在这里勾选。
          </p>
        {/if}

        {#if step.kind === 'ai' && skills.length}
          <details>
            <summary class="hint">带上技能（{step.skills.length || '无'}）</summary>
            <div class="skills">
              {#each skills as s (s.name)}
                <label>
                  <input
                    type="checkbox"
                    checked={step.skills.includes(s.name)}
                    onchange={(e) => {
                      step.skills = e.currentTarget.checked
                        ? [...step.skills, s.name]
                        : step.skills.filter((n) => n !== s.name);
                    }}
                  />
                  <span><b>{s.name}</b> <em>{s.description}</em></span>
                </label>
              {/each}
            </div>
          </details>
        {/if}
      </div>
    </article>
  {/each}
</div>

<div class="add">
  <span class="hint">加一步</span>
  {#each KINDS as k (k.id)}
    <button class="btn-sm" onclick={() => add(k.id)} title={k.hint}>
      <Icon name="plus" />
      {k.label}
    </button>
  {/each}
</div>

<style>
  .steps {
    display: flex;
    flex-direction: column;
  }
  .step {
    display: grid;
    grid-template-columns: 2rem 1fr;
    gap: var(--s3);
  }
  /* 左侧的序号和竖线：一眼看出这是有顺序的，不是一堆并列的卡片 */

  .body {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s3) var(--s4);
    margin-bottom: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    min-width: 0;
    transition: border-color var(--dur-2) var(--ease);
  }
  .body:focus-within {
    border-color: var(--line-strong);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--s1);
  }
  .title {
    font-size: var(--t-md);
    font-weight: 500;
    border: none;
    background: transparent;
    padding: 0;
  }
  .title:focus-visible {
    outline: none;
    box-shadow: none;
    border-bottom: 1px solid var(--accent);
    border-radius: 0;
  }
  textarea {
    width: 100%;
    font-family: var(--font);
    font-size: var(--t-base);
    line-height: 1.6;
  }
  textarea.mono {
    font-family: var(--mono);
    font-size: var(--t-sm);
  }
  .opts {
    display: flex;
    gap: var(--s2);
    flex-wrap: wrap;
  }
  .opts label {
    flex: 1;
    min-width: 8rem;
  }
  .opts .narrow {
    flex: 0 0 6.5rem;
    min-width: 6.5rem;
  }
  .hint {
    margin: 0;
    font-size: var(--t-xs);
    color: var(--fg-faint);
    line-height: 1.5;
  }
  .sees {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding-left: var(--s2);
    border-left: 2px solid var(--line-strong);
  }
  .chips {
    display: flex;
    gap: var(--s1);
    flex-wrap: wrap;
  }
  .chip {
    color: var(--fg-faint);
    max-width: 14rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chip.on {
    border-color: var(--accent-border);
    color: var(--accent-fg);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .warn-text {
    color: color-mix(in srgb, var(--warn) 80%, var(--fg-faint));
  }
  details summary {
    cursor: pointer;
  }
  .skills {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding-top: var(--s2);
  }
  .skills label {
    display: flex;
    gap: var(--s2);
    align-items: baseline;
    font-size: var(--t-sm);
  }
  .skills em {
    font-style: normal;
    color: var(--fg-faint);
  }
  .add {
    display: flex;
    gap: var(--s2);
    align-items: center;
    flex-wrap: wrap;
    padding: var(--s3);
    margin-left: calc(2rem + var(--s3));
    border: 1px dashed var(--line-strong);
    border-radius: var(--r3);
  }
</style>
