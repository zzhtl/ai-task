// 任务与 run 的接口封装。类型全部来自 ts-rs 生成的 ./types/。

import { api } from './client';
import type { CreateTask } from './types/CreateTask';
import type { Page } from './types/Page';
import type { RunStatus } from './types/RunStatus';
import type { RunSummary } from './types/RunSummary';
import type { TaskSummary } from './types/TaskSummary';
import type { TriggerRun } from './types/TriggerRun';

export function listTasks(signal?: AbortSignal): Promise<Page<TaskSummary>> {
  return api('/api/v1/tasks', { signal });
}

/** 翻页取全时最多拿这么多。任务是人配的，到这个量级说明该先清理了。 */
export const ALL_TASKS_CAP = 1000;

/**
 * 全部任务（按游标翻到底，封顶 [`ALL_TASKS_CAP`]）。
 *
 * 以前各处只拿第一页（默认 50 条）还把游标扔了：第 51 个任务在任务页搜不到、
 * 执行记录的任务筛选里没有、审批卡上显示不出任务名——而界面上看不出被截断了。
 */
export async function listAllTasks(signal?: AbortSignal): Promise<TaskSummary[]> {
  const out: TaskSummary[] = [];
  let cursor: string | null = null;
  do {
    const params = new URLSearchParams({ limit: '200' });
    if (cursor) params.set('cursor', cursor);
    const page: Page<TaskSummary> = await api(`/api/v1/tasks?${params}`, { signal });
    out.push(...page.items);
    cursor = page.next_cursor ?? null;
  } while (cursor && out.length < ALL_TASKS_CAP);
  return out;
}

/**
 * 幂等键。
 *
 * **每次调用现生成一个 UUID 是没用的**——之前就是这么写的，注释还写着
 * "网络重试不该产生两个任务"。但双击按钮会走两次函数、拿到两个不同的键，
 * 服务端照样建两个。键必须由**一次用户意图**决定，重试时复用同一个。
 *
 * 调用方在意图开始时拿一个键，失败重试时把它原样传回来。
 */
export function newIdempotencyKey(): string {
  return crypto.randomUUID();
}

export function createTask(body: CreateTask, idempotencyKey?: string): Promise<TaskSummary> {
  return api('/api/v1/tasks', { method: 'POST', body, idempotencyKey });
}

export interface RunListQuery {
  limit?: number;
  taskId?: string | null;
  /** 只要这些状态。空数组会得到空结果——"筛选条件为空"不等于"不筛"。 */
  status?: RunStatus[] | null;
  /** 上一页的 `next_cursor`。 */
  cursor?: string | null;
}

export function listRuns(
  query: number | RunListQuery = 20,
  signal?: AbortSignal
): Promise<Page<RunSummary>> {
  const q: RunListQuery = typeof query === 'number' ? { limit: query } : query;
  const params = new URLSearchParams();
  params.set('limit', String(q.limit ?? 50));
  if (q.taskId) params.set('task_id', q.taskId);
  if (q.status) params.set('status', q.status.join(','));
  if (q.cursor) params.set('cursor', q.cursor);
  return api(`/api/v1/runs?${params}`, { signal });
}

export function getRun(id: string): Promise<RunSummary> {
  return api(`/api/v1/runs/${id}`);
}

export function triggerRun(
  taskId: string,
  body: TriggerRun = { dry_run: false },
  idempotencyKey?: string
): Promise<RunSummary> {
  return api(`/api/v1/tasks/${taskId}/runs`, { method: 'POST', body, idempotencyKey });
}

export function cancelRun(id: string): Promise<void> {
  return api(`/api/v1/runs/${id}/cancel`, { method: 'POST' });
}

export function deleteRun(id: string): Promise<void> {
  return api(`/api/v1/runs/${id}`, { method: 'DELETE' });
}

/** 一份能直接跑起来的最小任务定义，用于首页的"新建"。 */
export function sampleTask(name: string): CreateTask {
  return {
    name,
    spec: {
      nodes: [
        {
          key: 'probe',
          config: {
            kind: 'ai',
            prompt: '统计当前目录下所有 .rs 文件的总行数，只回答数字。',
            executor: 'claude_code',
            model: 'claude-haiku-4-5',
            budget_usd: '0.200000'
          },
          retry: { max_attempts: 1, backoff_ms: 1000, backoff_factor: 2, feed_error_to_model: true },
          on_failure: 'fail_fast'
        }
      ],
      edges: []
    },
    enabled: true
  };
}
