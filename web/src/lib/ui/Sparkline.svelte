<script lang="ts">
  /**
   * 指标卡底部的迷你趋势。
   *
   * 线用弱化色，最后一天用强调色的点标出来：要看的是"最近和之前比怎么样"。
   * 没有数据的日子（比如那天一次都没跑完）断开，不画成 0——0% 和"没跑"是两回事。
   *
   * 悬停只是顺手看一眼；每个值在首页"每天的执行结果"的表格里都有，不靠悬停才能读到。
   */
  import { dayLabel } from '$lib/charts/daily';

  let {
    points,
    format,
    label,
    domain,
    height = 36
  }: {
    points: Array<{ date: string; value: number | null }>;
    format: (value: number) => string;
    /** 读屏念的整体描述，比如"近 14 天每天的成功率"。 */
    label: string;
    /** 纵轴范围。成功率固定 [0, 1]；不给就从 0 到最大值。 */
    domain?: [number, number];
    height?: number;
  } = $props();

  let width = $state(0);
  let active = $state<number | null>(null);

  // 给末端的点（半径 4 + 2px 描边）留出位置，不被 svg 边缘裁掉
  const PAD = 6;

  const range = $derived.by((): [number, number] => {
    if (domain) return domain;
    const max = Math.max(0, ...points.map((p) => p.value ?? 0));
    return [0, max > 0 ? max : 1];
  });
  const x = (i: number) =>
    points.length > 1 ? PAD + (i * (width - 2 * PAD)) / (points.length - 1) : width / 2;
  const y = (v: number) => {
    const [lo, hi] = range;
    return height - PAD - ((v - lo) / (hi - lo || 1)) * (height - 2 * PAD);
  };

  /** 连续有值的一段一条线，下面垫一层淡色。 */
  const segments = $derived.by(() => {
    const out: Array<{ line: string; area: string }> = [];
    let run: number[] = [];
    const flush = () => {
      if (run.length === 0) return;
      const pts = run.map((i) => `${x(i).toFixed(1)},${y(points[i].value ?? 0).toFixed(1)}`);
      const line = `M${pts.join('L')}`;
      const base = (height - PAD).toFixed(1);
      const area = `${line}L${x(run[run.length - 1]).toFixed(1)},${base}L${x(run[0]).toFixed(1)},${base}Z`;
      out.push({ line, area });
      run = [];
    };
    points.forEach((p, i) => {
      if (p.value === null) flush();
      else run.push(i);
    });
    flush();
    return out;
  });

  const last = $derived.by(() => {
    for (let i = points.length - 1; i >= 0; i--) if (points[i].value !== null) return i;
    return null;
  });

  function onMove(event: PointerEvent) {
    if (points.length === 0 || width === 0) return;
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const step = points.length > 1 ? (width - 2 * PAD) / (points.length - 1) : 1;
    const i = Math.round((event.clientX - rect.left - PAD) / step);
    active = Math.max(0, Math.min(points.length - 1, i));
  }

  const tip = $derived.by(() => {
    if (active === null) return null;
    const p = points[active];
    const d = dayLabel(p.date);
    return {
      text: `${d.short} ${d.weekday} · ${p.value === null ? '没有数据' : format(p.value)}`,
      // 提示框贴着十字线，但不跑出卡片
      left: Math.max(0, Math.min(width - 120, x(active) - 60))
    };
  });
</script>

<div
  class="spark"
  style="height: {height}px"
  bind:clientWidth={width}
  onpointermove={onMove}
  onpointerleave={() => (active = null)}
  role="presentation"
>
  {#if width > 0}
    <svg {width} {height} role="img" aria-label={label}>
      {#each segments as seg, i (i)}
        <path class="area" d={seg.area} />
        <path class="line" d={seg.line} />
      {/each}
      {#if active !== null}
        <line class="cross" x1={x(active)} x2={x(active)} y1={0} y2={height} />
        {#if points[active].value !== null}
          <circle class="hover-dot" cx={x(active)} cy={y(points[active].value ?? 0)} r="3" />
        {/if}
      {/if}
      {#if last !== null}
        <circle class="end" cx={x(last)} cy={y(points[last].value ?? 0)} r="4" />
      {/if}
    </svg>
  {/if}
  {#if tip}
    <span class="tip" style="left: {tip.left}px">{tip.text}</span>
  {/if}
</div>

<style>
  .spark {
    position: relative;
    width: 100%;
  }
  svg {
    display: block;
    overflow: visible;
  }
  .line {
    fill: none;
    stroke: var(--viz-other);
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .area {
    fill: var(--viz-other);
    opacity: 0.1;
  }
  .end {
    fill: var(--accent);
    stroke: var(--surface-1);
    stroke-width: 2;
  }
  .hover-dot {
    fill: var(--fg-dim);
    stroke: var(--surface-1);
    stroke-width: 2;
  }
  .cross {
    stroke: var(--line-strong);
    stroke-width: 1;
  }
  .tip {
    position: absolute;
    bottom: calc(100% + 4px);
    width: 120px;
    padding: 2px 6px;
    border: 1px solid var(--line-strong);
    border-radius: var(--r1);
    background: var(--surface-2);
    box-shadow: var(--shadow-pop);
    font-size: var(--t-xs);
    color: var(--fg);
    text-align: center;
    white-space: nowrap;
    pointer-events: none;
    z-index: 1;
  }
</style>
