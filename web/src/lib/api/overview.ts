// 首页的几个聚合接口。类型全部来自 ts-rs 生成的 ./types/。
import { api } from './client';
import type { Overview } from './types/Overview';
import type { OverviewDaily } from './types/OverviewDaily';
import type { OverviewTasks } from './types/OverviewTasks';

export const getOverview = (signal?: AbortSignal) => api<Overview>('/api/v1/overview', { signal });

/** 最近几天每天的执行结果，按 `timezone` 的日历日分桶。 */
export const getOverviewDaily = (days: number, timezone: string, signal?: AbortSignal) =>
  api<OverviewDaily>(
    `/api/v1/overview/daily?${new URLSearchParams({ days: String(days), timezone })}`,
    { signal }
  );

/** 窗口内失败最多的任务。 */
export const getFailingTasks = (windowHours: number, limit: number, signal?: AbortSignal) =>
  api<OverviewTasks>(
    `/api/v1/overview/tasks?${new URLSearchParams({ window_hours: String(windowHours), limit: String(limit) })}`,
    { signal }
  );
