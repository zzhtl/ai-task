// 任务与 run 的接口封装。类型全部来自 ts-rs 生成的 ./types/。

import { api } from './client';
import type { CreateTask } from './types/CreateTask';
import type { Page } from './types/Page';
import type { RunDetail } from './types/RunDetail';
import type { RunListItem } from './types/RunListItem';
import type { RunStatus } from './types/RunStatus';
import type { RunSummary } from './types/RunSummary';
import type { TaskDetail } from './types/TaskDetail';
import type { TaskSummary } from './types/TaskSummary';
import type { TaskVersion } from './types/TaskVersion';
import type { TriggerKind } from './types/TriggerKind';
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

export const getTask = (id: string, signal?: AbortSignal) =>
  api<TaskDetail>(`/api/v1/tasks/${id}`, { signal });

/**
 * 停用 / 启用。停用的任务：定时到点不触发，手动也触发不了。
 *
 * PUT 是整体替换，所以先取当前定义原样带回去，只翻 enabled 这一位；
 * 带着取到的版本号做 If-Match，取和写之间被别人改过就是 412，不会把对方的改动冲掉。
 */
export async function setTaskEnabled(id: string, enabled: boolean): Promise<void> {
  const task = await getTask(id);
  await api(`/api/v1/tasks/${id}`, {
    method: 'PUT',
    body: {
      name: task.name,
      description: task.description ?? null,
      spec: task.spec,
      rules: task.rules ?? [],
      enabled
    },
    ifMatch: String(task.version)
  });
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
  /** 只要这些触发方式。 */
  trigger?: TriggerKind[] | null;
  /** 只要这个时刻（含）之后创建的。 */
  since?: Date | null;
  /** 上一页的 `next_cursor`。 */
  cursor?: string | null;
}

export function listRuns(
  query: number | RunListQuery = 20,
  signal?: AbortSignal
): Promise<Page<RunListItem>> {
  const q: RunListQuery = typeof query === 'number' ? { limit: query } : query;
  const params = new URLSearchParams();
  params.set('limit', String(q.limit ?? 50));
  if (q.taskId) params.set('task_id', q.taskId);
  if (q.status) params.set('status', q.status.join(','));
  if (q.trigger) params.set('trigger', q.trigger.join(','));
  if (q.since) params.set('since', q.since.toISOString());
  if (q.cursor) params.set('cursor', q.cursor);
  return api(`/api/v1/runs?${params}`, { signal });
}

/** 一次执行的全部元信息：比列表多了任务名、版本号、输入输出、触发人。 */
export function getRun(id: string): Promise<RunDetail> {
  return api(`/api/v1/runs/${id}`);
}

/** 版本不可变：取过一次就一直用，同一页里来回切不再发请求。 */
const versions = new Map<string, Promise<TaskVersion>>();

/**
 * 任务某个版本的编排快照。执行详情用它画**这次执行用的那一版**——
 * 任务改过之后，拿当前定义去画老 run，图和步骤名都是错的。
 */
export function getTaskVersion(taskId: string, versionNo: number): Promise<TaskVersion> {
  const key = `${taskId}@${versionNo}`;
  let hit = versions.get(key);
  if (!hit) {
    hit = api<TaskVersion>(`/api/v1/tasks/${taskId}/versions/${versionNo}`);
    // 失败的不缓存：下次还要能重试
    hit.catch(() => versions.delete(key));
    versions.set(key, hit);
  }
  return hit;
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
