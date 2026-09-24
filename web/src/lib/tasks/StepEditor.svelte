<script lang="ts">
  /**
   * 有序步骤编辑器。
   *
   * 每一步说清楚：做什么、在哪台机器上、谁来做。前后自动串起来，
   * **每步都能看见上一步的结果**——不串的话「按顺序」就没有意义。
   *
   * 步骤多了之后整页都是表单，找不到要改的那一步：每张卡片都能收起成一行摘要，
   * 卡片之间能插入、能复制，顺序用按钮或 Alt+↑/↓ 调（拖拽对键盘和触屏都不友好）。
   */
  import { tick } from 'svelte';
  import { newStep, reachOf, REACH_TOOLS, type Composition, type Reach, type Step } from './compose';
  import type { Problem } from './problems';
  import Icon from '$lib/ui/Icon.svelte';
  import StepRail from '$lib/ui/StepRail.svelte';
  import { autogrow } from '$lib/ui/autogrow';
  import { scrollToElement } from '$lib/ui/scroll';
  // 这三个类型在 models.ts 里有唯一定义。之前这里各抄了一份：
  // 后端加字段时抄出去的那份不会报错，只会悄悄对不上。
  import type { Host, Skill } from '$api/models';

  let {
    comp = $bindable(),
    hosts,
    skills,
    problems = []
  }: { comp: Composition; hosts: Host[]; skills: Skill[]; problems?: Problem[] } = $props();

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
  const KIND_LABEL = Object.fromEntries(KINDS.map((k) => [k.id, k.label])) as Record<Step['kind'], string>;

  const clisOf = (hostId: string | null) =>
    hostId ? (hosts.find((h) => h.id === hostId)?.ai_clis ?? []) : [];
  const hostLabel = (id: string | null) =>
    id ? (hosts.find((h) => h.id === id)?.name ?? id.slice(0, 8)) : '本机';

  // ---------------------------------------------------------------- 收起 / 定位

  /** 收起的步骤。只是这一页的显示状态，不进编排。 */
  let collapsed = $state<Record<string, boolean>>({});
  const allCollapsed = $derived(comp.steps.length > 0 && comp.steps.every((s) => collapsed[s.uid]));
  function collapseAll(value: boolean) {
    collapsed = Object.fromEntries(comp.steps.map((s) => [s.uid, value]));
  }

  /** 页面上点了某条问题：展开那一步，滚过去。 */
  export async function reveal(uid: string) {
    collapsed[uid] = false;
    await tick();
    const card = document.querySelector(`[data-step="${CSS.escape(uid)}"]`);
    scrollToElement(card);
    // 光标直接放进这一步的正文（审批步骤没有正文就放标题），省一次点击
    card?.querySelector<HTMLElement>('textarea, input.title')?.focus({ preventScroll: true });
  }

  const problemsOf = (uid: string) => problems.filter((p) => p.stepUid === uid);

  /** 收起时的一行摘要：够认出是哪一步就行。 */
  function summaryOf(step: Step): string {
    const where = hostLabel(step.hostId);
    if (step.kind === 'approval') return `在${where}停下来等人确认，${step.timeoutS ?? 900} 秒没人管按拒绝`;
    const first = step.body.trim().split('\n')[0] || '（还没写）';
    return `${where} · ${first}`;
  }

  // ---------------------------------------------------------------- 增删改顺序

  /** 顺序变化要念出来：看不见屏幕的人不知道 Alt+↓ 之后这一步到了哪儿。 */
  let announce = $state('');
  let inserting = $state<number | null>(null);

  async function focusTitle(uid: string) {
    await tick();
    document.querySelector<HTMLInputElement>(`[data-step="${CSS.escape(uid)}"] .title`)?.focus();
  }

  function insert(at: number, kind: Step['kind']) {
    const step = newStep(kind);
    // 默认接着上一步。大多数流程是线性的，每加一步都要手动勾一次太烦
    const prev = comp.steps[at - 1];
    if (prev) step.sees = [prev.uid];
    comp.steps = [...comp.steps.slice(0, at), step, ...comp.steps.slice(at)];
    inserting = null;
    announce = `在第 ${at + 1} 步加了「${step.title}」`;
    void focusTitle(step.uid);
  }

  /** 复制一步放在它后面。新的那份没有 key，保存时拿一个不冲突的。 */
  function duplicate(i: number) {
    const src = $state.snapshot(comp.steps[i]) as Step;
    const copy: Step = { ...src, uid: newStep(src.kind).uid, key: undefined, title: `${src.title} 副本` };
    comp.steps = [...comp.steps.slice(0, i + 1), copy, ...comp.steps.slice(i + 1)];
    announce = `复制了「${src.title}」，放在第 ${i + 2} 步`;
    void focusTitle(copy.uid);
  }

  function remove(i: number) {
    const gone = comp.steps[i];
    comp.steps = comp.steps.filter((_, at) => at !== i);
    announce = `删掉了「${gone.title || '未命名'}」`;
  }

  async function move(i: number, delta: number) {
    const to = i + delta;
    if (to < 0 || to >= comp.steps.length) return;
    // 键盘挪完焦点还得在原来的地方，否则连按两下 Alt+↓ 就挪不动了
    const active = document.activeElement as HTMLElement | null;
    const next = [...comp.steps];
    [next[i], next[to]] = [next[to], next[i]];
    comp.steps = next;
    announce = `「${next[to].title || '未命名'}」移到了第 ${to + 1} 步`;
    await tick();
    if (active?.isConnected && document.activeElement !== active) active.focus();
  }

  function onCardKey(event: KeyboardEvent, i: number) {
    if (!event.altKey || (event.key !== 'ArrowUp' && event.key !== 'ArrowDown')) return;
    event.preventDefault();
    void move(i, event.key === 'ArrowUp' ? -1 : 1);
  }

  /** 这一步之前的所有步骤。只能看更早的——看后面的就是循环依赖。 */
  const earlier = (i: number) => comp.steps.slice(0, i);

  function toggleSees(step: Step, uid: string) {
    step.sees = step.sees.includes(uid) ? step.sees.filter((u) => u !== uid) : [...step.sees, uid];
  }

  /** 换机器之后，原来选的 CLI 可能在新机器上不存在，退回中心执行。 */
  function onHostChange(step: Step) {
    if (step.runner.kind !== 'host_cli') return;
    const wanted = step.runner.cli;
    // 新机器上没有原来那个 CLI 就退回中心执行，别留一个必然失败的配置
    if (!clisOf(step.hostId).some((c) => c.name === wanted)) step.runner = { kind: 'center' };
  }

  const placeholder = (kind: Step['kind']) =>
    kind === 'ai'
      ? '用自然语言说清楚要做什么。比如：读 /var/log/app.log，统计最近一天的 ERROR 行数，判断是否异常。只读，不要改任何文件。'
      : kind === 'shell'
        ? 'systemctl restart nginx'
        : '';

  /** 高级设置里改过的，折叠着也要看得见。 */
  function advancedSummary(step: Step): string {
    const parts: string[] = [];
    if (step.maxAttempts > 1) parts.push(`失败重试 ${step.maxAttempts - 1} 次`);
    if (step.kind === 'ai') {
      parts.push(step.maxTurns === null ? '轮数不限' : `最多 ${step.maxTurns} 轮`);
      if (step.budgetUsd) parts.push(`单步最多 $${step.budgetUsd}`);
    }
    return parts.length ? `：${parts.join(' · ')}` : '';
  }
</script>

<div class="toolbar-row">
  <span class="hint">{comp.steps.length} 步 · Alt+↑/↓ 调整顺序</span>
  <span class="spacer"></span>
  {#if comp.steps.length > 1}
    <button class="btn-ghost btn-sm" onclick={() => collapseAll(!allCollapsed)}>
      {allCollapsed ? '全部展开' : '全部收起'}
    </button>
  {/if}
</div>

<div class="steps">
  {#each comp.steps as step, i (step.uid)}
    {@const mine = problemsOf(step.uid)}
    {@const errors = mine.filter((p) => p.level === 'error').length}
    {@const isCollapsed = collapsed[step.uid] ?? false}

    {#if i > 0}
      <!-- 卡片之间的插入口。竖线接着左边的序号线走，看得出是插在两步之间 -->
      <div class="gap">
        <span class="gap-wire" aria-hidden="true"></span>
        {#if inserting === i}
          <div class="insert-choices">
            <span class="hint">插入一步：</span>
            {#each KINDS as k (k.id)}
              <button class="btn-sm" title={k.hint} onclick={() => insert(i, k.id)}>{k.label}</button>
            {/each}
            <button class="btn-ghost btn-sm" onclick={() => (inserting = null)}>取消</button>
          </div>
        {:else}
          <button
            class="insert btn-ghost btn-sm"
            aria-label="在第 {i} 步和第 {i + 1} 步之间插入一步"
            onclick={() => (inserting = i)}
          >
            <Icon name="plus" size={12} />
            <span>插入</span>
          </button>
        {/if}
      </div>
    {/if}

    <!-- 键盘排序挂在整张卡片上：焦点在卡片里任何地方都能 Alt+↑/↓ -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <article class="step {step.kind}" data-step={step.uid} onkeydown={(e) => onCardKey(e, i)}>
      <StepRail index={i + 1} last={i === comp.steps.length - 1} kind={step.kind} />

      <div class="body" class:collapsed={isCollapsed} class:has-error={errors > 0}>
        <header>
          <button
            class="btn-ghost btn-sm btn-icon"
            aria-expanded={!isCollapsed}
            aria-label={isCollapsed ? '展开这一步' : '收起这一步'}
            title={isCollapsed ? '展开' : '收起'}
            onclick={() => (collapsed[step.uid] = !isCollapsed)}
          >
            <Icon name={isCollapsed ? 'chevron-right' : 'chevron-down'} />
          </button>
          {#if isCollapsed}<span class="tag kind-tag">{KIND_LABEL[step.kind]}</span>{/if}
          <input class="title" bind:value={step.title} placeholder="这一步叫什么" aria-label="第 {i + 1} 步的名字" />
          {#if errors}
            <span class="tag danger">{errors} 个问题</span>
          {:else if mine.length}
            <span class="tag warn" title={mine.map((p) => p.text).join('\n')}>提醒</span>
          {/if}
          <span class="actions">
            <button class="btn-ghost btn-sm btn-icon" onclick={() => duplicate(i)} title="复制这一步" aria-label="复制这一步">
              <Icon name="copy" />
            </button>
            <button
              class="btn-ghost btn-sm btn-icon"
              disabled={i === 0}
              onclick={() => move(i, -1)}
              title="上移（Alt+↑）"
              aria-label="上移"
            >
              <Icon name="arrow-up" />
            </button>
            <button
              class="btn-ghost btn-sm btn-icon"
              disabled={i === comp.steps.length - 1}
              onclick={() => move(i, 1)}
              title="下移（Alt+↓）"
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
              <Icon name="trash" />
            </button>
          </span>
        </header>

        {#if isCollapsed}
          <p class="summary ellipsis" class:mono={step.kind === 'shell'}>{summaryOf(step)}</p>
        {:else}
          <div class="seg" role="group" aria-label="这一步做什么">
            {#each KINDS as k (k.id)}
              <button class:on={step.kind === k.id} title={k.hint} onclick={() => (step.kind = k.id)}>
                {k.label}
              </button>
            {/each}
          </div>

          {#if step.kind !== 'approval'}
            <textarea
              bind:value={step.body}
              rows={step.kind === 'ai' ? 4 : 2}
              placeholder={placeholder(step.kind)}
              spellcheck="false"
              class:mono={step.kind === 'shell'}
              aria-label="第 {i + 1} 步{step.kind === 'ai' ? '的提示词' : '要运行的命令'}"
              {@attach autogrow}
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
                    aria-pressed={step.sees.includes(prior.uid)}
                    title={prior.title || `第 ${at + 1} 步`}
                    onclick={() => toggleSees(step, prior.uid)}
                  >
                    {at + 1} · {prior.title || '未命名'}
                  </button>
                {/each}
              </div>
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
              {step.kind === 'approval' ? '等多久（秒）' : '超时（秒）'}
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
              ——Bash/Write 会直接落地，中心只看得见输出；影子执行也拦不住它。
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
          {:else if step.kind === 'ai'}
            <p class="hint">
              还没有导入过技能。技能是一份写给 AI 的操作手册（怎么查这类问题、
              公司内部的命令怎么敲），在<a href="/rules">规则页</a>导入后可以在这里勾选。
            </p>
          {/if}

          {#if step.kind !== 'approval'}
            <details class="adv">
              <summary class="hint">高级{advancedSummary(step)}</summary>
              <div class="opts">
                <label class="field narrow">
                  失败后重试
                  <select
                    value={String(step.maxAttempts - 1)}
                    onchange={(e) => (step.maxAttempts = Number(e.currentTarget.value) + 1)}
                  >
                    <option value="0">不重试</option>
                    {#each [1, 2, 3, 4] as n (n)}<option value={String(n)}>{n} 次</option>{/each}
                  </select>
                </label>
                {#if step.kind === 'ai'}
                  <label class="field narrow">
                    最多几轮
                    <input
                      type="number"
                      min="1"
                      max="200"
                      placeholder="不限"
                      value={step.maxTurns ?? ''}
                      oninput={(e) => {
                        const v = e.currentTarget.value;
                        step.maxTurns = v === '' ? null : Number(v);
                      }}
                    />
                  </label>
                  <label class="field">
                    这一步最多花（美元）
                    <input
                      inputmode="decimal"
                      placeholder="不单独限制"
                      value={step.budgetUsd ?? ''}
                      oninput={(e) => {
                        const v = e.currentTarget.value.trim();
                        step.budgetUsd = v === '' ? null : v;
                      }}
                    />
                  </label>
                {/if}
              </div>
              {#if step.kind === 'ai'}
                <p class="hint">重试时会把上一次的失败原因告诉模型。单步上限只管这一步，整个任务还有总上限。</p>
              {/if}
            </details>
          {/if}

          {#if mine.length}
            <ul class="problems">
              {#each mine as p (p.text)}
                <li class:error={p.level === 'error'}>{p.text}</li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>
    </article>
  {/each}
</div>

<div class="add">
  <span class="hint">在最后加一步</span>
  {#each KINDS as k (k.id)}
    <button class="btn-sm" onclick={() => insert(comp.steps.length, k.id)} title={k.hint}>
      <Icon name="plus" />
      {k.label}
    </button>
  {/each}
</div>

<p class="sr-only" aria-live="polite">{announce}</p>

<style>
  .toolbar-row {
    display: flex;
    align-items: center;
    gap: var(--s2);
    margin-bottom: var(--s2);
  }
  .steps {
    display: flex;
    flex-direction: column;
  }
  .step {
    display: grid;
    grid-template-columns: 2rem minmax(0, 1fr);
    gap: var(--s3);
  }
  .body {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    box-shadow: var(--shadow-card);
    padding: var(--s3) var(--s4);
    margin-bottom: var(--s2);
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    min-width: 0;
    transition: border-color var(--dur-2) var(--ease);
  }
  .body:focus-within {
    border-color: var(--line-strong);
  }
  .body.collapsed {
    gap: var(--s1);
    padding-block: var(--s2);
  }
  .body.has-error {
    border-color: var(--bad-border);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--s2);
    margin-left: calc(-1 * var(--s2));
  }
  .kind-tag {
    flex: 0 0 auto;
  }
  /* 带上父级：暗色主题的全局 input 规则特异性是 (0,2,1)，单个类压不过它 */
  header input.title {
    flex: 1;
    min-width: 6rem;
    font-size: var(--t-md);
    font-weight: 600;
    border: none;
    background: transparent;
    box-shadow: none;
    padding: 0.2rem 0;
  }
  header input.title:focus-visible {
    outline: none;
    box-shadow: none;
    border-bottom: 1px solid var(--accent);
    border-radius: 0;
  }
  .actions {
    display: flex;
    gap: 2px;
    flex: 0 0 auto;
  }
  .summary {
    margin: 0 0 0 calc(1.75rem + var(--s1));
    font-size: var(--t-sm);
    color: var(--fg-faint);
  }
  .seg {
    align-self: flex-start;
  }
  textarea {
    width: 100%;
    min-height: 4.5rem;
    max-height: 60vh;
    resize: vertical;
    font-family: var(--font);
    font-size: var(--t-base);
    line-height: 1.6;
  }
  textarea.mono {
    font-family: var(--mono);
    font-size: var(--t-sm);
    min-height: 2.75rem;
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
    flex: 0 0 7rem;
    min-width: 7rem;
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
    background: var(--accent-soft);
  }
  details summary {
    cursor: pointer;
  }
  .adv .opts {
    margin-top: var(--s2);
  }
  .adv .hint:last-child {
    margin-top: var(--s2);
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
  .problems {
    margin: 0;
    padding-left: 1.1rem;
    font-size: var(--t-xs);
    color: var(--warn-fg);
  }
  .problems li.error {
    color: var(--bad-fg);
  }

  /* 两张卡片之间：接着左边的竖线，中间一个平时很淡的"插入" */
  .gap {
    display: grid;
    grid-template-columns: 2rem minmax(0, 1fr);
    gap: var(--s3);
    align-items: center;
    min-height: 1.75rem;
    margin: calc(-1 * var(--s1)) 0 var(--s1);
  }
  .gap-wire {
    justify-self: center;
    align-self: stretch;
    width: 1px;
    background: var(--line);
  }
  .insert {
    justify-self: start;
    opacity: 0.35;
    color: var(--fg-faint);
  }
  .gap:hover .insert,
  .insert:focus-visible {
    opacity: 1;
  }
  .insert-choices {
    display: flex;
    align-items: center;
    gap: var(--s1);
    flex-wrap: wrap;
  }

  .add {
    display: flex;
    gap: var(--s2);
    align-items: center;
    flex-wrap: wrap;
    padding: var(--s3);
    margin: var(--s2) 0 0 calc(2rem + var(--s3));
    border: 1px dashed var(--line-strong);
    border-radius: var(--r3);
  }
</style>
