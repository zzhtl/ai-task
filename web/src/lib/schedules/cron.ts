// 定时的"人话"：cron 表达式 ⇄ 界面上的几种常见写法。
//
// 手写 cron 是配定时最容易出错的地方：`0 0 * * *` 和 `0 0 * * 0` 只差一个字符，
// 一个每天跑、一个每周日跑。所以界面默认用构建器（每天 / 每周 / 每月 / 间隔），
// 构建器表示不了的才落到自定义表达式；列表里也尽量把表达式翻成人话。
//
// **只认本模块自己生成得出来的那几种形状。**认不出的原样显示——猜错一个表达式的
// 意思，比不翻译更糟。

export type CronBuilder =
  | { mode: 'daily'; time: string }
  /** `days` 用 cron 的编号：0 = 周日，1 = 周一 … 6 = 周六。 */
  | { mode: 'weekly'; days: number[]; time: string }
  | { mode: 'monthly'; day: number | 'last'; time: string }
  | { mode: 'interval'; every: number; unit: IntervalUnit }
  | { mode: 'custom'; expr: string };

export type IntervalUnit = 'second' | 'minute' | 'hour';

/**
 * 间隔只给能整除的档位。`*\/7` 在 cron 里不是"每 7 分钟"：到整点会从 0 重新数，
 * 55 分之后下一次是 00 分，只隔 5 分钟。能整除的档位才名副其实。
 */
export const INTERVALS: Record<IntervalUnit, number[]> = {
  second: [5, 10, 15, 20, 30],
  minute: [1, 2, 3, 5, 10, 15, 20, 30],
  hour: [1, 2, 3, 4, 6, 8, 12]
};
const DIVIDES: Record<IntervalUnit, number> = { second: 60, minute: 60, hour: 24 };

const pad = (n: number) => String(n).padStart(2, '0');

/** `HH:MM` → [时, 分]。填得不对返回 null。 */
function hm(time: string): [number, number] | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(time.trim());
  if (!m) return null;
  const h = Number(m[1]);
  const min = Number(m[2]);
  return h < 24 && min < 60 ? [h, min] : null;
}

/** 星期列表写成 cron：排好序去重，连续三天以上写成区间（1-5 比 1,2,3,4,5 好读）。 */
function dowField(days: number[]): string {
  const sorted = [...new Set(days)].sort((a, b) => a - b);
  const parts: string[] = [];
  for (let i = 0; i < sorted.length; ) {
    let j = i;
    while (j + 1 < sorted.length && sorted[j + 1] === sorted[j] + 1) j += 1;
    if (j - i >= 2) parts.push(`${sorted[i]}-${sorted[j]}`);
    else for (let k = i; k <= j; k++) parts.push(String(sorted[k]));
    i = j + 1;
  }
  return parts.join(',');
}

/** 构建器 → 表达式。没填完的返回空串：宁可不让保存，也不能悄悄变成"每天"。 */
export function toCron(b: CronBuilder): string {
  if (b.mode === 'custom') return b.expr.trim();
  if (b.mode === 'interval') {
    if (b.unit === 'second') return `*/${b.every} * * * * *`;
    if (b.unit === 'minute') return b.every === 1 ? '* * * * *' : `*/${b.every} * * * *`;
    return b.every === 1 ? '0 * * * *' : `0 */${b.every} * * *`;
  }
  const t = hm(b.time);
  if (!t) return '';
  const [h, m] = t;
  if (b.mode === 'daily') return `${m} ${h} * * *`;
  if (b.mode === 'weekly') return b.days.length ? `${m} ${h} * * ${dowField(b.days)}` : '';
  return `${m} ${h} ${b.day === 'last' ? 'L' : b.day} * *`;
}

/** 只认纯数字的分和时。`*\/5`、`1,31` 这些都不算"几点几分"。 */
function clockOf(min: string, hour: string): string | null {
  if (!/^\d{1,2}$/.test(min) || !/^\d{1,2}$/.test(hour)) return null;
  const m = Number(min);
  const h = Number(hour);
  return h < 24 && m < 60 ? `${pad(h)}:${pad(m)}` : null;
}

/** 星期字段：只认数字和数字区间，7 也当周日。名字（MON）、`#`、`L` 都不认。 */
function daysOf(field: string): number[] | null {
  const out = new Set<number>();
  for (const part of field.split(',')) {
    const range = /^(\d)(?:-(\d))?$/.exec(part);
    if (!range) return null;
    const from = Number(range[1]);
    const to = range[2] === undefined ? from : Number(range[2]);
    if (from > 7 || to > 7 || to < from) return null;
    for (let d = from; d <= to; d++) out.add(d % 7);
  }
  return [...out].sort((a, b) => a - b);
}

function everyOf(field: string, unit: IntervalUnit): number | null {
  const m = /^\*\/(\d+)$/.exec(field);
  if (!m) return null;
  const n = Number(m[1]);
  return n > 0 && DIVIDES[unit] % n === 0 ? n : null;
}

/** 表达式 → 构建器。认不出的一律是自定义。 */
export function fromCron(expr: string): CronBuilder {
  const text = expr.trim();
  const custom: CronBuilder = { mode: 'custom', expr: text };
  const f = text.split(/\s+/);

  if (f.length === 6) {
    const every = everyOf(f[0], 'second');
    return every && f.slice(1).every((x) => x === '*') ? { mode: 'interval', every, unit: 'second' } : custom;
  }
  if (f.length !== 5) return custom;
  const [min, hour, dom, mon, dow] = f;
  if (mon !== '*') return custom;

  if (dom === '*' && dow === '*') {
    if (min === '*' && hour === '*') return { mode: 'interval', every: 1, unit: 'minute' };
    const everyMin = everyOf(min, 'minute');
    if (everyMin && hour === '*') return { mode: 'interval', every: everyMin, unit: 'minute' };
    if (min === '0' && hour === '*') return { mode: 'interval', every: 1, unit: 'hour' };
    const everyHour = everyOf(hour, 'hour');
    if (min === '0' && everyHour) return { mode: 'interval', every: everyHour, unit: 'hour' };
  }

  const time = clockOf(min, hour);
  if (!time) return custom;
  if (dom === '*' && dow === '*') return { mode: 'daily', time };
  if (dom === '*') {
    const days = daysOf(dow);
    if (!days) return custom;
    return days.length === 7 ? { mode: 'daily', time } : { mode: 'weekly', days, time };
  }
  // 日和星期同时写时 cron 取"或"，一句人话讲不清，交给自定义
  if (dow !== '*') return custom;
  if (dom === 'L') return { mode: 'monthly', day: 'last', time };
  if (/^\d{1,2}$/.test(dom) && Number(dom) >= 1 && Number(dom) <= 31) {
    return { mode: 'monthly', day: Number(dom), time };
  }
  return custom;
}

const DAY_NAMES = ['日', '一', '二', '三', '四', '五', '六'];
const UNIT_NAMES: Record<IntervalUnit, string> = { second: '秒', minute: '分钟', hour: '小时' };

function describeDays(days: number[]): string {
  const key = days.join(',');
  if (key === '1,2,3,4,5') return '工作日';
  if (key === '0,6') return '周末';
  // 周一排在前面、周日排最后，才是中文里说"周几"的顺序
  const ordered = [...days].sort((a, b) => ((a + 6) % 7) - ((b + 6) % 7));
  return `每周${ordered.map((d) => DAY_NAMES[d]).join('、')}`;
}

export function describeBuilder(b: CronBuilder): string {
  switch (b.mode) {
    case 'daily':
      return `每天 ${b.time}`;
    case 'weekly':
      return `${describeDays(b.days)} ${b.time}`;
    case 'monthly':
      return b.day === 'last' ? `每月最后一天 ${b.time}` : `每月 ${b.day} 号 ${b.time}`;
    case 'interval':
      return b.every === 1 ? `每${UNIT_NAMES[b.unit]}` : `每 ${b.every} ${UNIT_NAMES[b.unit]}`;
    case 'custom':
      return b.expr;
  }
}

/** 列表和预览里用：认得出就说人话，认不出原样给表达式。 */
export function describeCron(expr: string): string {
  return describeBuilder(fromCron(expr));
}
