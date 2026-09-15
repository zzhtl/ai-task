<script lang="ts">
  /**
   * 归因到本 run 的资源曲线。
   *
   * 数据是 cgroup 里这次执行的**累计量**，不是机器级读数——同一台机器上跑着
   * 别的东西也不会混进来。CPU 是累计微秒，必须相邻两点做差才是使用率；
   * 直接画累计值只会得到一条单调上升、看不出任何问题的线。
   */
  import { api, describeError } from '$api/client';

  interface MetricPoint {
    at: string;
    cpu_usec: number;
    rss_bytes: number;
    pids: number;
  }
  interface NodeMetrics {
    node_key: string;
    points: MetricPoint[];
  }

  let {
    runId,
    revision = 0,
    degraded = {} as Record<string, { mode: string; detail: string | null }>
  }: {
    runId: string;
    revision?: number;
    degraded?: Record<string, { mode: string; detail: string | null }>;
  } = $props();

  let series = $state<NodeMetrics[]>([]);
  /** 采样读不出来。不是 admin 门禁的接口，所以这里失败就是真故障。 */
  let error = $state<string | null>(null);

  $effect(() => {
    // revision 变了就重取：事件流告诉我们节点跑完了，这里不用自己轮询
    void revision;
    if (!runId) return;
    api<NodeMetrics[]>(`/api/v1/runs/${runId}/metrics`)
      .then((s) => {
        series = s;
        error = null;
      })
      // 之前这里是 `.catch(() => {})`：采样读失败时整块曲线连同标题一起消失，
      // 和"这个 run 本来就没有资源采样"长得一模一样。
      .catch((e) => (error = describeError(e)));
  });

  const W = 720;
  const H = 90;
  const PAD = 4;

  /** 累计 CPU 微秒 → 每个采样区间的使用率（100 = 一个核跑满）。 */
  function cpuPercent(points: MetricPoint[]): { t: number; v: number }[] {
    const out: { t: number; v: number }[] = [];
    for (let i = 1; i < points.length; i++) {
      const dtMs = Date.parse(points[i].at) - Date.parse(points[i - 1].at);
      if (dtMs <= 0) continue;
      const dCpu = points[i].cpu_usec - points[i - 1].cpu_usec;
      out.push({ t: Date.parse(points[i].at), v: Math.max(0, (dCpu / 1000 / dtMs) * 100) });
    }
    return out;
  }

  /**
   * 所有节点共用一根时间轴。
   *
   * 每张图各自缩放的话，串行跑的两个节点会画成一样宽——看起来像同时发生的。
   * 共用之后横向位置就是"什么时候"，和上面的 DAG、下面的事件流对得上。
   */
  const domain = $derived.by(() => {
    let lo = Infinity;
    let hi = -Infinity;
    for (const s of series) {
      for (const p of s.points) {
        const t = Date.parse(p.at);
        if (t < lo) lo = t;
        if (t > hi) hi = t;
      }
    }
    return hi > lo ? { lo, hi } : null;
  });

  function path(pts: { t: number; v: number }[], max: number): string {
    const d = domain;
    if (!d || pts.length === 0 || max <= 0) return '';
    return pts
      .map((p, i) => {
        const x = PAD + ((p.t - d.lo) / (d.hi - d.lo)) * (W - 2 * PAD);
        const y = H - PAD - (p.v / max) * (H - 2 * PAD);
        return `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(' ');
  }

  /**
   * 求最大值。**不能用 `Math.max(...arr)`**：参数展开会把整个数组推上调用栈，
   * V8 大约在十万个元素时抛 RangeError。1 Hz 采样跑够久就能撞上，而那正是
   * 最需要看曲线的时候。采样条数由服务端决定，前端不该被一个大响应打崩。
   */
  function peak<T>(items: T[], of: (item: T) => number): number {
    let max = 0;
    for (const item of items) {
      const v = of(item);
      if (v > max) max = v;
    }
    return max;
  }

  const charts = $derived.by(() =>
    series.map((s) => {
      const cpu = cpuPercent(s.points);
      const rss = s.points.map((p) => ({ t: Date.parse(p.at), v: p.rss_bytes }));
      // 上界向上取整到一个好看的刻度，否则每来一个点整条线都会跳
      const cpuMax = Math.max(100, Math.ceil(peak(cpu, (p) => p.v) / 100) * 100);
      const rssMax = Math.max(1, peak(rss, (p) => p.v));
      return {
        key: s.node_key,
        cpuPath: path(cpu, cpuMax),
        rssPath: path(rss, rssMax),
        cpuMax,
        peakRss: rssMax,
        peakPids: peak(s.points, (p) => p.pids),
        degraded: degraded[s.node_key],
        // 这个节点在共用时间轴上占的区间：底色 + 表头的起止偏移
        span: (() => {
          const d = domain;
          if (!d || s.points.length === 0) return null;
          const lo = Date.parse(s.points[0].at);
          const hi = Date.parse(s.points[s.points.length - 1].at);
          return {
            x: PAD + ((lo - d.lo) / (d.hi - d.lo)) * (W - 2 * PAD),
            w: Math.max(1, ((hi - lo) / (d.hi - d.lo)) * (W - 2 * PAD)),
            from: (lo - d.lo) / 1000,
            to: (hi - d.lo) / 1000
          };
        })()
      };
    })
  );

  const totalSeconds = $derived(domain ? (domain.hi - domain.lo) / 1000 : 0);
</script>

{#if error}
  <div class="banner">读不到资源采样：{error}</div>
{:else if charts.length > 0}
  <section class="metrics">
    <h2>
      资源归因
      <span class="muted">共用时间轴，全长 {totalSeconds.toFixed(1)}s</span>
    </h2>
    {#each charts as c (c.key)}
      <article>
        <header>
          <span class="node mono">{c.key}</span>
          <span class="muted">峰值 {(c.peakRss / 1048576).toFixed(1)} MiB · {c.peakPids} 进程</span>
          {#if c.span}
            <span class="muted">+{c.span.from.toFixed(1)}s → +{c.span.to.toFixed(1)}s</span>
          {/if}
          {#if c.degraded}
            <!-- 降级要显式说：这一档下 MemoryMax/CPUQuota 根本没有被强制 -->
            <span class="degraded" title={c.degraded.detail ?? ''}>
              降级采样（{c.degraded.mode}）· 上限未强制
            </span>
          {/if}
        </header>
        <svg
          viewBox="0 0 {W} {H}"
          preserveAspectRatio="none"
          role="img"
          aria-label="{c.key} 的 CPU 与内存曲线"
        >
          {#if c.span}
            <!-- 底色标出这个节点实际占用的时间段，空白处不是"零"而是"没在跑" -->
            <rect class="span" x={c.span.x} y="0" width={c.span.w} height={H} />
          {/if}
          <path class="rss" d={c.rssPath} />
          <path class="cpu" d={c.cpuPath} />
        </svg>
        <footer class="muted">
          <span class="swatch cpu"></span>CPU（满刻度 {c.cpuMax}%）
          <span class="swatch rss"></span>RSS（满刻度 {(c.peakRss / 1048576).toFixed(1)} MiB）
        </footer>
      </article>
    {/each}
  </section>
{/if}

<style>
  .metrics {
    margin-top: var(--s4);
  }
  h2 {
    margin: 0 0 var(--s3);
  }
  article {
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: var(--s3) var(--s4);
    background: var(--surface-1);
  }
  article + article {
    margin-top: var(--s2);
  }
  header {
    display: flex;
    gap: var(--s3);
    align-items: baseline;
    font-size: var(--t-base);
    flex-wrap: wrap;
  }
  .node {
    color: var(--fg);
  }
  svg {
    width: 100%;
    height: 90px;
    display: block;
    margin: 0.35rem 0 0.2rem;
  }
  path {
    fill: none;
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }
  path.cpu {
    stroke: var(--accent);
  }
  path.rss {
    stroke: var(--ok);
    opacity: 0.75;
  }
  rect.span {
    fill: var(--line);
    opacity: 0.35;
  }
  h2 .muted {
    font-weight: 400;
    font-size: var(--t-sm);
    margin-left: 0.5rem;
  }
  .muted {
    color: var(--fg-faint);
  }
  .degraded {
    color: var(--warn);
    font-size: var(--t-sm);
  }
  footer {
    font-size: var(--t-xs);
    display: flex;
    gap: 0.4rem;
    align-items: center;
    color: var(--fg-faint);
  }
  .swatch {
    display: inline-block;
    width: 0.75rem;
    height: 2px;
  }
  .swatch.cpu {
    background: var(--accent);
  }
  .swatch.rss {
    background: var(--ok);
    margin-left: 0.75rem;
  }
</style>
