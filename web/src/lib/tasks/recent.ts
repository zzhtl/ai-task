// 任务列表里"最近几次"的统计口径。
import type { RecentRun } from '$api/types/RecentRun';
import { FAILED_STATUSES } from '$lib/ui/format';

/**
 * 最近几次里成功了几次、有结论的一共几次。
 *
 * 只算**真跑出了结论**的：影子执行是在试提示词，还没跑完的没有结论，
 * 被人手动取消的也说明不了任务稳不稳。把它们算进分母，成功率就在讲别的事了。
 */
export function successOf(runs: RecentRun[]): { ok: number; done: number } {
  const done = runs.filter(
    (r) => !r.dry_run && (r.status === 'succeeded' || FAILED_STATUSES.includes(r.status))
  );
  return { ok: done.filter((r) => r.status === 'succeeded').length, done: done.length };
}
