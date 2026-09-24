// 首页"每天的执行结果"的计算部分：刻度、分段、形状。画在 DailyChart / Sparkline 里。
import type { DailyBucket } from '$api/types/DailyBucket';

/** 纵轴上限往上取到 1 / 2 / 5 × 10ⁿ，刻度才是干净的整数。至少为 1：全是 0 也要有个轴。 */
export function niceMax(value: number): number {
  if (value <= 1) return 1;
  const exp = 10 ** Math.floor(Math.log10(value));
  for (const step of [1, 2, 5, 10]) {
    if (step * exp >= value) return step * exp;
  }
  return 10 * exp;
}

/** 0、中间、最大。中间那格不是整数就不画——"2.5 次执行"没有意义。 */
export function ticksFor(max: number): number[] {
  return max > 1 && max % 2 === 0 ? [0, max / 2, max] : [0, max];
}

export type SegmentKey = 'failed' | 'succeeded' | 'other';

/**
 * 从下往上：失败、成功、其它。失败贴着基线，天与天之间最好比——
 * 首页要看的就是哪天坏得多。
 */
export function segmentsOf(b: DailyBucket): Array<{ key: SegmentKey; value: number }> {
  return [
    { key: 'failed', value: b.failed },
    { key: 'succeeded', value: b.succeeded },
    { key: 'other', value: Math.max(0, b.runs - b.succeeded - b.failed) }
  ];
}

/** 有结论的里面成功了多少。一次都没结束的那天是 `null`，画成断开，而不是 0%。 */
export function successRate(b: DailyBucket): number | null {
  const done = b.succeeded + b.failed;
  return done === 0 ? null : b.succeeded / done;
}

const WEEKDAYS = ['周日', '周一', '周二', '周三', '周四', '周五', '周六'];

/** `2026-09-24` → `09-24` + `周四`。按日期本身算星期，不经过浏览器时区。 */
export function dayLabel(date: string): { short: string; weekday: string } {
  const [y, m, d] = date.split('-').map(Number);
  return { short: date.slice(5), weekday: WEEKDAYS[new Date(Date.UTC(y, m - 1, d)).getUTCDay()] };
}

/** 一根柱子（或一段）的路径：上面两个角圆，底边方。圆角不超过高度和半宽。 */
export function topRoundedBar(x: number, y: number, w: number, h: number, r: number): string {
  const rr = Math.max(0, Math.min(r, h, w / 2));
  const bottom = y + h;
  return (
    `M${x},${bottom}V${y + rr}Q${x},${y} ${x + rr},${y}` +
    `H${x + w - rr}Q${x + w},${y} ${x + w},${y + rr}V${bottom}Z`
  );
}
