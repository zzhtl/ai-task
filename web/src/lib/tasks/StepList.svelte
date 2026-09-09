<script lang="ts">
  /**
   * 只读的步骤列表：这个任务按顺序会做什么。
   *
   * 和编辑器共用同一套视觉语言（左侧序号 + 连线），这样"看"和"改"是同一件
   * 事的两个状态，而不是两套需要各自学习的东西。
   */
  import { reachOf, type Composition, type Step } from './compose';

  let {
    comp,
    hosts = [],
    selected = null
  }: {
    comp: Composition;
    hosts?: Array<{ id: string; name: string }>;
    /** 画布上选中的节点 key，用来高亮对应的那一步。 */
    selected?: string | null;
  } = $props();

  const KIND_LABEL: Record<Step['kind'], string> = {
    ai: 'AI 执行',
    shell: '运行命令',
    approval: '人工确认'
  };

  const REACH_LABEL: Record<ReturnType<typeof reachOf>, string> = {
    read_only: '只读',
    run_commands: '可执行命令',
    edit_files: '可改文件'
  };

  // 主机是 admin 才能读，读不到就退回短 id
  const hostLabel = (id: string | null) =>
    id ? (hosts.find((h) => h.id === id)?.name ?? id.slice(0, 8)) : '本机';

  const runnerLabel = (step: Step) =>
    step.runner.kind === 'host_cli' ? `目标机上的 ${step.runner.cli}` : '中心的 Claude Code';
</script>

<ol class="steps">
  {#each comp.steps as step, i (step.uid)}
    <li class="step {step.kind}" class:on={selected === `step-${i + 1}`}>
      <div class="rail">
        <span class="num">{i + 1}</span>
        {#if i < comp.steps.length - 1}<span class="wire"></span>{/if}
      </div>

      <div class="body">
        <header>
          <span class="title">{step.title}</span>
          <span class="tag">{KIND_LABEL[step.kind]}</span>
        </header>

        {#if step.kind === 'approval'}
          <p class="hint">停下来等人在审批页点头。超时按拒绝处理。</p>
        {:else}
          <pre class:mono={step.kind === 'shell'}>{step.body}</pre>
        {/if}

        {#if step.sees.length}
          <p class="sees">
            看得到：{step.sees
              .map((uid) => {
                const at = comp.steps.findIndex((s) => s.uid === uid);
                return at < 0 ? null : `${at + 1} · ${comp.steps[at].title || '未命名'}`;
              })
              .filter(Boolean)
              .join('，')} 的结果
          </p>
        {/if}

        <div class="facts">
          <span>在 <b>{hostLabel(step.hostId)}</b></span>
          {#if step.kind === 'ai'}
            <span>{runnerLabel(step)}</span>
            <!-- title 里放真实清单：档位只是概括，真正生效的是这几个工具名 -->
            <span title={step.tools.join('、') || 'CLI 默认工具'}>{REACH_LABEL[reachOf(step.tools)]}</span>
            <!-- 没指定模型是要说出来的：fingerprint 里含模型，不定的话漂移检测会失真 -->
            {#if step.model}<span class="mono">{step.model}</span>{:else}<span class="warn-text">模型未指定</span>{/if}
            {#if step.skills.length}<span>技能：{step.skills.join('、')}</span>{/if}
          {/if}
          <span>{step.timeoutS}s 超时</span>
        </div>
      </div>
    </li>
  {/each}
</ol>

<style>
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .step {
    display: grid;
    grid-template-columns: 2rem 1fr;
    gap: var(--s3);
  }
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s1);
  }
  .num {
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 50%;
    border: 1px solid var(--line-strong);
    background: var(--surface-2);
    display: grid;
    place-items: center;
    font-size: 0.78rem;
    color: var(--fg-dim);
    flex: 0 0 auto;
  }
  .wire {
    flex: 1;
    width: 1px;
    background: var(--line);
    min-height: var(--s4);
  }
  .step.ai .num {
    border-color: color-mix(in srgb, #56b6f5 55%, var(--line-strong));
    color: #7cc4f8;
  }
  .step.approval .num {
    border-color: color-mix(in srgb, var(--warn) 55%, var(--line-strong));
    color: var(--warn);
  }

  .body {
    background: var(--surface-1);
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s3);
    margin-bottom: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    transition: border-color 0.12s ease;
  }
  /* 画布上点了哪个节点，这里就亮哪一步 */
  .step.on .body {
    border-color: var(--accent-dim);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }
  .title {
    font-size: 0.95rem;
    font-weight: 500;
  }
  pre {
    margin: 0;
    font-family: var(--font);
    font-size: 0.86rem;
    line-height: 1.65;
    color: var(--fg-dim);
    /* 提示词是整段自然语言，要换行显示全，不要横向滚动条 */
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  pre.mono {
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .facts {
    display: flex;
    gap: var(--s3);
    flex-wrap: wrap;
    font-size: 0.76rem;
    color: var(--fg-faint);
  }
  .warn-text {
    color: var(--warn);
  }
  .facts b {
    color: var(--fg-dim);
    font-weight: 500;
  }
  .hint {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-faint);
  }
  .sees {
    margin: 0;
    padding-left: var(--s2);
    border-left: 2px solid var(--line-strong);
    font-size: 0.76rem;
    color: var(--fg-faint);
  }
</style>
