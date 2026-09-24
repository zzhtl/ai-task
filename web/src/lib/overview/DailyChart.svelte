<script lang="ts">
  /**
   * 近几天每天的执行结果：失败 / 成功 / 其它堆成一根柱子，失败贴着基线。
   *
   * 成功、失败本身就带着好坏的意思，所以用状态色而不是分类色；"其它"（取消、还在跑的）
   * 用弱化灰。这组颜色过了 dataviz 的校验（色盲模拟下相邻两段 ΔE ≥ 8）。
   * 颜色之外还有图例、分段之间的空隙、悬停/键盘提示和表格视图，不单靠颜色传达。
   */
  import type { DailyBucket } from '$api/types/DailyBucket';
  import { dayLabel, niceMax, segmentsOf, successRate, ticksFor, topRoundedBar, type SegmentKey } from '$lib/charts/daily';
  import { money } from '$lib/ui/format';

  let { days }: { days: DailyBucket[] } = $props();

  const NAMES: Record<SegmentKey, string> = { failed: '失败', succeeded: '成功', other: '其它' };
  const ORDER: SegmentKey[] = ['failed', 'succeeded', 'other'];

  let width = $state(0);
  let active = $state<number | null>(null);
  let asTable = $state(false);

  const PLOT_H = 150;
  const TOP = 10;
  const AXIS_W = 32;
  const AXIS_H = 24;
  const GAP = 2;
  const MIN_SEG = 2;

  const max = $derived(niceMax(Math.max(0, ...days.map((d) => d.runs))));
  const ticks = $derived(ticksFor(max));
  const plotW = $derived(Math.max(0, width - AXIS_W));
  const slot = $derived(days.length ? plotW / days.length : 0);
  const barW = $derived(Math.min(24, Math.max(6, slot * 0.55)));
  /** 窄的时候隔一天标一个日期，免得挤在一起。最后一天（今天）总是标。 */
  const labelEvery = $derived(slot < 44 ? 2 : 1);
  const y = (v: number) => TOP + PLOT_H - (v / max) * PLOT_H;

  const totals = $derived(
    Object.fromEntries(
      ORDER.map((key) => [
        key,
        days.reduce((sum, d) => sum + (segmentsOf(d).find((s) => s.key === key)?.value ?? 0), 0)
      ])
    ) as Record<SegmentKey, number>
  );
  const empty = $derived(days.every((d) => d.runs === 0));

  /** 每天一根柱子：从基线往上一段段叠，段与段之间留 2px 空隙，最上面那段圆角。 */
  const columns = $derived(
    days.map((d, i) => {
      const x = AXIS_W + i * slot + (slot - barW) / 2;
      const parts = segmentsOf(d).filter((s) => s.value > 0);
      let bottom = TOP + PLOT_H;
      const segs = parts.map((s, at) => {
        const h = Math.max(MIN_SEG, (s.value / max) * PLOT_H - (at === 0 ? 0 : GAP));
        const top = bottom - h;
        const shape =
          at === parts.length - 1
            ? { kind: 'path' as const, d: topRoundedBar(x, top, barW, h, 4) }
            : { kind: 'rect' as const, x, y: top, w: barW, h };
        bottom = top - GAP;
        return { key: s.key, shape };
      });
      return { x, segs };
    })
  );

  const describe = (d: DailyBucket) => {
    const l = dayLabel(d.date);
    const rate = successRate(d);
    const segs = segmentsOf(d);
    const parts = segs.map((s) => `${NAMES[s.key]} ${s.value}`).join('，');
    return `${l.short} ${l.weekday}：${parts}${rate === null ? '' : `，成功率 ${Math.round(rate * 100)}%`}，花费 ${money(d.spend_usd)}`;
  };

  function onKey(event: KeyboardEvent) {
    if (days.length === 0) return;
    const at = active ?? days.length - 1;
    const go = (i: number) => {
      event.preventDefault();
      active = Math.max(0, Math.min(days.length - 1, i));
    };
    if (event.key === 'ArrowLeft') go(active === null ? at : at - 1);
    else if (event.key === 'ArrowRight') go(active === null ? at : at + 1);
    else if (event.key === 'Home') go(0);
    else if (event.key === 'End') go(days.length - 1);
    else if (event.key === 'Escape') active = null;
  }

  /** 提示放在那一列的旁边而不是正上方：盖住正在看的柱子就白看了。 */
  const tip = $derived.by(() => {
    if (active === null || !days[active]) return null;
    const d = days[active];
    const left = AXIS_W + active * slot;
    return {
      day: d,
      label: dayLabel(d.date),
      rate: successRate(d),
      style: left + slot / 2 < width / 2 ? `left: ${left + slot + 4}px` : `right: ${width - left + 4}px`
    };
  });
</script>

<div class="head">
  <ul class="legend" aria-label="图例">
    {#each ORDER as key (key)}
      <li><span class="swatch {key}"></span>{NAMES[key]} <b>{totals[key]}</b></li>
    {/each}
  </ul>
  <button
    class="btn-ghost btn-sm"
    aria-pressed={asTable}
    onclick={() => (asTable = !asTable)}
  >
    {asTable ? '看图' : '看表格'}
  </button>
</div>

{#if asTable}
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>日期</th>
          <th class="num">失败</th>
          <th class="num">成功</th>
          <th class="num">其它</th>
          <th class="num">成功率</th>
          <th class="num">花费</th>
        </tr>
      </thead>
      <tbody>
        {#each [...days].reverse() as d (d.date)}
          {@const l = dayLabel(d.date)}
          {@const rate = successRate(d)}
          {@const segs = segmentsOf(d)}
          <tr>
            <td class="nowrap">{l.short} <span class="faint">{l.weekday}</span></td>
            {#each segs as s (s.key)}<td class="num">{s.value}</td>{/each}
            <td class="num">{rate === null ? '—' : `${Math.round(rate * 100)}%`}</td>
            <td class="num">{money(d.spend_usd)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{:else if empty}
  <p class="empty-note">这段时间没有执行。</p>
{:else}
  <!--
    键盘：聚焦后左右方向键逐天看，和悬停看到的是同一份提示。
    "在几天里来回挑一天看"就是 slider 的键盘模型；读屏会自动切到焦点模式、
    每移一天念一遍 valuetext。完整的数字在表格视图里。
  -->
  <div
    class="chart"
    bind:clientWidth={width}
    role="slider"
    aria-label="每天的执行结果"
    aria-valuemin={1}
    aria-valuemax={days.length}
    aria-valuenow={(active ?? days.length - 1) + 1}
    aria-valuetext={describe(days[active ?? days.length - 1])}
    tabindex="0"
    onkeydown={onKey}
    onfocus={() => (active ??= days.length - 1)}
    onpointerleave={(e) => {
      // 触屏抬手也算 leave：点一下就该看住，点别处（失焦）再收起
      if (e.pointerType !== 'touch') active = null;
    }}
    onblur={() => (active = null)}
  >
    {#if width > 0}
      <svg {width} height={TOP + PLOT_H + AXIS_H} aria-hidden="true">
        {#each ticks as t (t)}
          <line class="grid" x1={AXIS_W} x2={width} y1={y(t)} y2={y(t)} />
          <text class="tick" x={AXIS_W - 6} y={y(t)} dy="0.32em" text-anchor="end">{t}</text>
        {/each}
        {#if active !== null}
          <rect class="band" x={AXIS_W + active * slot} y={TOP} width={slot} height={PLOT_H} />
        {/if}
        {#each columns as col, i (days[i].date)}
          <g class="col" class:dim={active !== null && active !== i}>
            {#each col.segs as seg (seg.key)}
              {#if seg.shape.kind === 'path'}
                <path class="piece {seg.key}" d={seg.shape.d} />
              {:else}
                <rect class="piece {seg.key}" x={seg.shape.x} y={seg.shape.y} width={barW} height={seg.shape.h} />
              {/if}
            {/each}
          </g>
          {#if i === days.length - 1 || (days.length - 1 - i) % labelEvery === 0}
            <text class="tick day" x={AXIS_W + i * slot + slot / 2} y={TOP + PLOT_H + 16} text-anchor="middle">
              {i === days.length - 1 ? '今天' : dayLabel(days[i].date).short}
            </text>
          {/if}
          <!-- 命中区是整条竖带，不是那几个像素的柱子 -->
          <rect
            class="hit"
            x={AXIS_W + i * slot}
            y={TOP}
            width={slot}
            height={PLOT_H + AXIS_H}
            role="presentation"
            onpointerenter={() => (active = i)}
          />
        {/each}
      </svg>
    {/if}
    {#if tip}
      <div class="tip" style={tip.style}>
        <div class="tip-title">{tip.label.short} {tip.label.weekday}</div>
        {#each segmentsOf(tip.day) as s (s.key)}
          <div class="tip-row"><span class="key {s.key}"></span><b>{s.value}</b><span>{NAMES[s.key]}</span></div>
        {/each}
        <div class="tip-foot">
          {tip.rate === null ? '没有结束的执行' : `成功率 ${Math.round(tip.rate * 100)}%`} · 花费 {money(tip.day.spend_usd)}
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    gap: var(--s3);
    margin-bottom: var(--s2);
  }
  .legend {
    display: flex;
    gap: var(--s4);
    flex-wrap: wrap;
    list-style: none;
    margin: 0;
    padding: 0;
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .legend li {
    display: inline-flex;
    align-items: center;
    gap: var(--s1);
  }
  .legend b {
    color: var(--fg);
    font-weight: 600;
  }
  .head button {
    margin-left: auto;
  }
  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
  }
  .swatch.failed,
  .key.failed {
    background: var(--viz-failed);
  }
  .swatch.succeeded,
  .key.succeeded {
    background: var(--viz-succeeded);
  }
  .swatch.other,
  .key.other {
    background: var(--viz-other);
  }

  .chart {
    position: relative;
    border-radius: var(--r2);
  }
  .chart:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  svg {
    display: block;
  }
  .grid {
    stroke: var(--line);
    stroke-width: 1;
  }
  .tick {
    fill: var(--fg-faint);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
  }
  .band {
    fill: var(--surface-2);
  }
  .piece.failed {
    fill: var(--viz-failed);
  }
  .piece.succeeded {
    fill: var(--viz-succeeded);
  }
  .piece.other {
    fill: var(--viz-other);
  }
  .col {
    transition: opacity var(--dur-1) var(--ease);
  }
  .col.dim {
    opacity: 0.45;
  }
  .hit {
    fill: transparent;
  }

  .tip {
    position: absolute;
    top: 0;
    width: max-content;
    min-width: 140px;
    padding: var(--s2) var(--s3);
    border: 1px solid var(--line-strong);
    border-radius: var(--r2);
    background: var(--surface-2);
    box-shadow: var(--shadow-pop);
    font-size: var(--t-xs);
    color: var(--fg-dim);
    pointer-events: none;
    z-index: 1;
  }
  .tip-title {
    color: var(--fg);
    font-weight: 600;
    margin-bottom: 2px;
  }
  .tip-row {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }
  .tip-row b {
    min-width: 2.5ch;
    color: var(--fg);
    font-size: var(--t-sm);
    font-variant-numeric: tabular-nums;
  }
  /* 提示里用一小段线做标识，不用色块：这么小的地方色块太抢 */
  .key {
    width: 10px;
    height: 3px;
    border-radius: 2px;
  }
  .tip-foot {
    white-space: nowrap;
    margin-top: 2px;
    padding-top: 2px;
    border-top: 1px solid var(--line);
  }
  .table-wrap {
    overflow-x: auto;
  }
  .empty-note {
    margin: var(--s4) 0;
    color: var(--fg-faint);
    font-size: var(--t-sm);
  }
</style>
