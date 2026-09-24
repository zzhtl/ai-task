<script lang="ts">
  /**
   * 状态徽标。**全站唯一的状态渲染方式**——列表、过程、编排图、页头必须完全一致。
   * 同一个状态在两个地方长得不一样，人就会停下来确认，那正是要消除的东西。
   *
   * 每种状态有自己的色调**和图标**：只靠颜色区分的话，色弱的人、投影仪、
   * 黑白截图都分不清"失败"和"超时"（WCAG 1.4.1）。
   *
   * - `badge`：淡底胶囊，表格和页头用；
   * - `text`：图标 + 彩色文字，不要底色，挤在一行里的列表用；
   * - `dot`：只剩图标，文字进 title 和读屏，一行放不下文字的地方用。
   */
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';

  let {
    status,
    label,
    variant = 'badge',
    size = 'md'
  }: {
    status: string;
    label?: string;
    variant?: 'badge' | 'text' | 'dot';
    size?: 'md' | 'lg';
  } = $props();

  type Tone = 'neutral' | 'info' | 'ok' | 'bad' | 'warn' | 'violet';

  const META: Record<string, { text: string; tone: Tone; icon: IconName }> = {
    queued: { text: '排队中', tone: 'neutral', icon: 'circle-dashed' },
    running: { text: '执行中', tone: 'info', icon: 'loader' },
    succeeded: { text: '成功', tone: 'ok', icon: 'check' },
    failed: { text: '失败', tone: 'bad', icon: 'close' },
    cancelled: { text: '已取消', tone: 'neutral', icon: 'ban' },
    timed_out: { text: '超时', tone: 'warn', icon: 'clock' },
    budget_exceeded: { text: '预算耗尽', tone: 'warn', icon: 'dollar' },
    resource_exceeded: { text: '资源击穿', tone: 'violet', icon: 'cpu' },
    awaiting_approval: { text: '等待审批', tone: 'warn', icon: 'pause' },
    pending: { text: '等待', tone: 'neutral', icon: 'circle' },
    ready: { text: '就绪', tone: 'neutral', icon: 'circle' },
    skipped: { text: '跳过', tone: 'neutral', icon: 'skip' },
    // 界面自己推出来的：run 结束了，这一步没等到结论
    interrupted: { text: '中断', tone: 'neutral', icon: 'ban' }
  };

  // 认不出的状态（后端新加的）照原样显示，不能崩也不能吞掉
  const meta = $derived(META[status] ?? { text: status, tone: 'neutral' as Tone, icon: 'circle' as IconName });
  const text = $derived(label ?? meta.text);
</script>

{#if variant === 'dot'}
  <span class="st dot {meta.tone}" class:spin={status === 'running'} title={text} role="img" aria-label={text}>
    <Icon name={meta.icon} />
  </span>
{:else}
  <span class="st {variant} {meta.tone} {size}" class:spin={status === 'running'}>
    <Icon name={meta.icon} />
    <span class="label">{text}</span>
  </span>
{/if}

<style>
  .st {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    white-space: nowrap;
    font-weight: 500;
    line-height: 1;
    vertical-align: middle;
  }
  .st :global(svg) {
    width: 12px;
    height: 12px;
    flex: 0 0 auto;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  /* 跑着的东西必须在动：静止的图标和静止的灰点一样没有信息量 */
  .spin :global(svg) {
    animation: spin 1.1s linear infinite;
  }

  .badge {
    height: 1.375rem;
    padding: 0 0.5rem 0 0.4rem;
    border-radius: 999px;
    border: 1px solid;
    font-size: var(--t-xs);
  }
  .badge.lg {
    height: 1.625rem;
    padding: 0 0.65rem 0 0.5rem;
    font-size: var(--t-sm);
  }
  .badge.lg :global(svg) {
    width: 14px;
    height: 14px;
  }
  .text {
    font-size: var(--t-sm);
  }
  .dot {
    width: 1.25rem;
    height: 1.25rem;
    justify-content: center;
    border-radius: 50%;
  }

  .neutral {
    color: var(--neutral-fg);
    border-color: var(--neutral-border);
  }
  .info {
    color: var(--info-fg);
    border-color: var(--info-border);
  }
  .ok {
    color: var(--ok-fg);
    border-color: var(--ok-border);
  }
  .bad {
    color: var(--bad-fg);
    border-color: var(--bad-border);
  }
  .warn {
    color: var(--warn-fg);
    border-color: var(--warn-border);
  }
  .violet {
    color: var(--violet-fg);
    border-color: var(--violet-border);
  }
  .badge.neutral,
  .dot.neutral {
    background: var(--neutral-bg);
  }
  .badge.info,
  .dot.info {
    background: var(--info-bg);
  }
  .badge.ok,
  .dot.ok {
    background: var(--ok-bg);
  }
  .badge.bad,
  .dot.bad {
    background: var(--bad-bg);
  }
  .badge.warn,
  .dot.warn {
    background: var(--warn-bg);
  }
  .badge.violet,
  .dot.violet {
    background: var(--violet-bg);
  }
</style>
