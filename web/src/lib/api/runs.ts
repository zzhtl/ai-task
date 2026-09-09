// 任务与 run 的接口封装。类型全部来自 ts-rs 生成的 ./types/。

import { api } from './client';
import type { CreateTask } from './types/CreateTask';
import type { Page } from './types/Page';
import type { RunSummary } from './types/RunSummary';
import type { TaskSummary } from './types/TaskSummary';
import type { TriggerRun } from './types/TriggerRun';

export function listTasks(): Promise<Page<TaskSummary>> {
  return api('/api/v1/tasks');
}

export function createTask(body: CreateTask): Promise<TaskSummary> {
  return api('/api/v1/tasks', {
    method: 'POST',
    body,
    // 创建类请求带幂等键：网络重试不该产生两个任务
    idempotencyKey: crypto.randomUUID()
  });
}

export function listRuns(limit = 20): Promise<Page<RunSummary>> {
  return api(`/api/v1/runs?limit=${limit}`);
}

export function getRun(id: string): Promise<RunSummary> {
  return api(`/api/v1/runs/${id}`);
}

export function triggerRun(taskId: string, body: TriggerRun = { dry_run: false }): Promise<RunSummary> {
  return api(`/api/v1/tasks/${taskId}/runs`, {
    method: 'POST',
    body,
    idempotencyKey: crypto.randomUUID()
  });
}

export function cancelRun(id: string): Promise<void> {
  return api(`/api/v1/runs/${id}/cancel`, { method: 'POST' });
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
