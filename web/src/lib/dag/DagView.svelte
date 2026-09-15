<script lang="ts">
  /**
   * 编排图。
   *
   * README 一直写着"在网页上看编排图并叠加实时执行状态"，而实际上界面里只有一条
   * 线性步骤条——`tasks/[id]` 甚至直接告诉用户分支/并行/map「无法展示」。
   * 一个编排工具画不出你刚编排的图，是核心职责上的残缺。
   *
   * 手写 SVG，零依赖。布局在 layout.ts 里（纯函数、可单测），这里只管画。
   */
  import { layout, CyclicDag, NODE_H, NODE_W, type Layout } from './layout';
  import type { DagSpec } from '$api/types/DagSpec';
  import StatusPill from '$lib/ui/StatusPill.svelte';

  let {
    spec,
    /** 节点 key -> 执行状态。不给就是纯编排视图。 */
    status = {},
    /** 点了某个节点。run 详情页用它滚到对应的过程段落。 */
    onselect
  }: {
    spec: DagSpec;
    status?: Record<string, string>;
    onselect?: (key: string) => void;
  } = $props();

  /** 节点太多时退回线性列表：一墙 4px 的小方块帮不了任何人。 */
  const MAX_NODES = 60;
  const MAX_LAYERS = 12;

  const model = $derived.by((): { layout: Layout } | { error: string } => {
    const nodes = (spec.nodes ?? []).map((n) => ({
      key: String(n.key),
      label: n.name ?? String(n.key),
      kind: kindOf(n)
    }));
    if (nodes.length === 0) return { error: '这个编排还没有节点。' };
    if (nodes.length > MAX_NODES) {
      return { error: `${nodes.length} 个节点，画成图反而看不清。下面的步骤列表更实用。` };
    }
    try {
      const out = layout({
        nodes,
        edges: (spec.edges ?? []).map((e) => ({
          from: String(e.from),
          to: String(e.to),
          condition: conditionLabel(e.when)
        }))
      });
      if (out.layers > MAX_LAYERS) {
        return { error: `${out.layers} 层太深，画成图反而看不清。` };
      }
      return { layout: out };
    } catch (e) {
      // 入库时已经校验过无环，这里兜底：报错总比转不出来强
      return { error: e instanceof CyclicDag ? e.message : '这个编排画不出来。' };
    }
  });

  function kindOf(node: DagSpec['nodes'][number]): string {
    const config = node.config as Record<string, unknown> | undefined;
    return typeof config?.kind === 'string' ? config.kind : 'ai';
  }

  /** 条件边要标出来，否则看不出"只在失败时走"这种分支。 */
  function conditionLabel(when: unknown): string | undefined {
    const op = (when as { op?: string } | undefined)?.op;
    if (!op || op === 'on_success') return undefined; // 默认条件不标，免得满图都是字
    if (op === 'on_failure') return '失败时';
    if (op === 'always') return '总是';
    return '条件';
  }

  /**
   * 把标签截到框里放得下。
   *
   * SVG 的 `<text>` 不会换行也不会省略号，长名字会直接画到框外面去——
   * 看起来像两个节点叠在一起。这里按"CJK 算两格、其它算一格"估宽度，
   * 不去做真实测量：measure 要先挂进 DOM，为一个标签不值得。
   * 完整名字放在 `<title>` 里，悬停看得到。
   */
  function fit(text: string, budget: number): string {
    let width = 0;
    let out = '';
    for (const ch of text) {
      const w = /[\u4e00-\u9fff\u3000-\u303f\uff00-\uffef]/.test(ch) ? 2 : 1;
      if (width + w > budget) return `${out}…`;
      width += w;
      out += ch;
    }
    return out;
  }

  /** 节点框内文字可用的宽度（格）。留出左右内边距和状态点的位置。 */
  const LABEL_BUDGET = 19;

  /** 从事件目标找回是哪个节点。返回 true 表示确实点中了一个。 */
  function pick(target: EventTarget | null): boolean {
    if (!onselect || !(target instanceof Element)) return false;
    const key = target.closest('[data-key]')?.getAttribute('data-key');
    if (!key) return false;
    onselect(key);
    return true;
  }

  const KIND_LABEL: Record<string, string> = {
    ai: 'AI',
    shell: '命令',
    approval: '审批',
    assert: '断言',
    map: '扇出'
  };
</script>

{#if 'error' in model}
  <p class="faint small">{model.error}</p>
{:else}
  {@const view = model.layout}
  <!-- 监听挂在外面这个 <div> 上，不挂在 <g> 上。
       实测 Svelte 5 的事件委托到不了 SVG 元素：`onclick` 写在 <g> 上时
       属性都渲染出来了（role / tabindex / aria-label 都在），但点上去没有任何反应，
       控制台也不报错。在 HTML 元素上收一次、再用 closest 找回是哪个节点，
       行为可控，而且焦点和键盘仍然落在每个 <g> 自己身上。 -->
  <div
    class="wrap"
    onclick={(event) => pick(event.target)}
    onkeydown={(event) => {
      if (event.key === 'Enter' || event.key === ' ') {
        if (pick(event.target)) event.preventDefault();
      }
    }}
    role="presentation"
  >
    <svg
      viewBox="0 0 {view.width} {view.height}"
      style="max-width: {view.width}px"
      role="img"
      aria-label="编排图：{view.nodes.length} 个节点、{view.layers} 层"
    >
      <defs>
        <marker
          id="dag-arrow"
          viewBox="0 0 8 8"
          refX="7"
          refY="4"
          markerWidth="6"
          markerHeight="6"
          orient="auto-start-reverse"
        >
          <path d="M 0 0 L 8 4 L 0 8 z" />
        </marker>
      </defs>

      {#each view.edges as e (e.from + '->' + e.to)}
        <path class="edge" class:conditional={!!e.condition} d={e.path} marker-end="url(#dag-arrow)" />
        {#if e.condition && e.labelAt}
          <text class="edge-label" x={e.labelAt.x} y={e.labelAt.y}>{e.condition}</text>
        {/if}
      {/each}

      <!-- 可点和不可点分两支写：把 role / tabindex 写成三元表达式的话，
           静态检查看不出它们是配套的，只能报 a11y 警告。 -->
      {#snippet box(n: (typeof view.nodes)[number])}
        <rect width={NODE_W} height={NODE_H} rx="10" />
        {#if status[n.key]}
          <circle class="dot {status[n.key]}" cx="14" cy="14" r="4" />
        {/if}
        <title>{n.label}</title>
        <text class="label" x={status[n.key] ? 26 : 12} y="19">
          {fit(n.label, status[n.key] ? LABEL_BUDGET - 2 : LABEL_BUDGET)}
        </text>
        <text class="kind" x={12} y="38">{KIND_LABEL[n.kind] ?? n.kind}</text>
      {/snippet}

      {#each view.nodes as n (n.key)}
        {#if onselect}
          <g
            class="node {n.kind} clickable"
            data-key={n.key}
            transform="translate({n.x},{n.y})"
            role="button"
            tabindex="0"
            aria-label="{n.label}（{KIND_LABEL[n.kind] ?? n.kind}）"
          >
            {@render box(n)}
          </g>
        {:else}
          <g class="node {n.kind}" transform="translate({n.x},{n.y})">
            {@render box(n)}
          </g>
        {/if}
      {/each}
    </svg>
  </div>

  {#if Object.keys(status).length}
    <div class="legend">
      {#each [...new Set(Object.values(status))] as s (s)}
        <StatusPill status={s} />
      {/each}
    </div>
  {/if}
{/if}

<style>
  .wrap {
    /* 窄屏上横向滚动，不把图压扁——压扁之后连线会拧成一团 */
    overflow-x: auto;
    padding: var(--s2) 0;
  }
  svg {
    width: 100%;
    height: auto;
    display: block;
    /* 左对齐，和下面的步骤条、页面其它内容一条基线。
       居中的话，单列编排会变成一大片空白里浮着的一小条。 */
  }
  .edge {
    fill: none;
    stroke: var(--line-strong);
    stroke-width: 1.5;
  }
  .edge.conditional {
    stroke-dasharray: 4 3;
    stroke: var(--warn);
  }
  #dag-arrow path {
    fill: var(--line-strong);
  }
  .edge-label {
    font-size: var(--t-2xs);
    fill: var(--warn);
    text-anchor: middle;
    paint-order: stroke;
    stroke: var(--bg);
    stroke-width: 3;
  }
  .node rect {
    fill: var(--surface-2);
    stroke: var(--line-strong);
    stroke-width: 1;
    transition: stroke var(--dur-2) var(--ease);
  }
  .node.ai rect {
    stroke: color-mix(in srgb, var(--st-ai) 55%, var(--line-strong));
  }
  .node.approval rect {
    stroke: color-mix(in srgb, var(--warn) 55%, var(--line-strong));
  }
  .node .label {
    fill: var(--fg);
    font-size: var(--t-sm);
    font-weight: 500;
  }
  .node .kind {
    fill: var(--fg-faint);
    font-size: var(--t-2xs);
  }
  .node.clickable {
    cursor: pointer;
  }
  .node.clickable:hover rect,
  .node.clickable:focus-visible rect {
    stroke: var(--accent);
  }
  .node.clickable:focus-visible {
    outline: none;
  }
  /* 状态点复用全站那套语义色，图上和列表里必须是同一个颜色 */
  .dot.running {
    fill: var(--st-running);
  }
  .dot.succeeded {
    fill: var(--st-succeeded);
  }
  .dot.failed,
  .dot.timed_out,
  .dot.budget_exceeded,
  .dot.resource_exceeded {
    fill: var(--st-failed);
  }
  .dot.awaiting_approval {
    fill: var(--st-awaiting);
  }
  .dot.skipped,
  .dot.pending,
  .dot.ready {
    fill: var(--st-skipped);
  }
  .legend {
    display: flex;
    gap: var(--s3);
    flex-wrap: wrap;
    padding-top: var(--s2);
  }
</style>
