// 格式化。全站共用，免得每页各写一份相对时间。

/** 相对时间。刻意粗粒度：秒级精度对"什么时候跑的"没有意义，还会一直重排。 */
export function ago(iso: string | null | undefined): string {
  if (!iso) return '—';
  const ms = Date.now() - Date.parse(iso);
  if (Number.isNaN(ms)) return '—';
  if (ms < 0) return '刚刚';
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s} 秒前`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} 分钟前`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} 小时前`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d} 天前`;
  return iso.slice(0, 10);
}

/** 两个时刻之间的墙钟耗时。 */
export function duration(from?: string | null, to?: string | null): string {
  if (!from) return '—';
  const ms = (to ? Date.parse(to) : Date.now()) - Date.parse(from);
  if (!Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${Math.floor(s % 60)}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

/**
 * 金额。后端给的是十进制字符串（不是浮点——累加会在几十次之后开始对不上账），
 * 这里只做显示层的截断。
 */
export function money(usd: string | number): string {
  const n = typeof usd === 'string' ? Number(usd) : usd;
  if (!Number.isFinite(n)) return '$0';
  if (n === 0) return '$0';
  return n < 0.01 ? `$${n.toFixed(4)}` : `$${n.toFixed(2)}`;
}

/** 字节。 */
export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(0)} KiB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MiB`;
  return `${(n / 1024 ** 3).toFixed(2)} GiB`;
}

const two = (n: number) => String(n).padStart(2, '0');

/**
 * 绝对时刻，**本地时区**，秒级。
 *
 * 以前是直接把 ISO 串里的 `T`/`Z` 抠掉——那显示的是 UTC，在东八区看
 * 每个时间都差 8 小时，排查时对不上日志。
 */
export function stamp(iso: string | null | undefined): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '—';
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())} ${clock(iso)}`;
}

/** 只有时分秒（本地时区）。事件流一行一条，日期在页头已经写着了。 */
export function clock(iso: string | null | undefined): string {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '—';
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}

/** `分:秒`，倒计时用。 */
export function mmss(total: number): string {
  const m = Math.floor(total / 60);
  return `${m}:${two(total % 60)}`;
}

/** UUID 的前 8 位。界面上够认，全量在 title / 复制里。 */
export function shortId(id: string): string {
  return id.slice(0, 8);
}

const TRIGGER: Record<string, string> = {
  manual: '手动',
  schedule: '定时',
  api: 'API',
  parent: '父 run'
};
export function triggerLabel(kind: string): string {
  return TRIGGER[kind] ?? kind;
}

/** 终态：不会再变了。 */
export function isTerminal(status: string): boolean {
  return status !== 'queued' && status !== 'running';
}
/** 算作"坏了"的那几个终态。 */
export const FAILED_STATUSES = ['failed', 'timed_out', 'budget_exceeded', 'resource_exceeded'];
