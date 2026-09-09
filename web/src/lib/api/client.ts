// 后端 API 的最小客户端。
//
// 类型全部来自 ./types/（由 `cargo test -p ai-task-proto` 从 Rust 生成），
// 不手写镜像。改 Rust 里的字段，这里会直接编译报错。

import type { ApiError } from './types/ApiError';

/** 后端返回的结构化错误。`code` 是契约，`message` 不是。 */
export class ApiFailure extends Error {
  constructor(
    readonly status: number,
    readonly body: ApiError
  ) {
    super(body.message);
    this.name = 'ApiFailure';
  }

  /** 便于在界面上按原因分支，而不是去匹配 message 文案。 */
  get code(): string {
    return this.body.code;
  }
}

/**
 * 把任何错误变成一句能贴在界面上的话。
 *
 * 校验失败时后端会**一次给全**字段级 details；只显示 message 的话，
 * 人看到的是"校验失败"四个字，然后得猜是哪个字段。
 */
export function describeError(e: unknown): string {
  if (e instanceof ApiFailure) {
    const details = e.body.details;
    if (details?.length) return details.map((d) => `${d.field}：${d.message}`).join('；');
    return e.message;
  }
  return e instanceof Error ? e.message : String(e);
}

export interface RequestOptions {
  method?: string;
  body?: unknown;
  signal?: AbortSignal;
  /** 超时（毫秒）。SSE 不走这个函数，所以这里可以设得比较短。 */
  timeoutMs?: number;
  /** 创建/触发类请求的幂等键。同一个键重放会拿回同一个结果。 */
  idempotencyKey?: string;
  /** 乐观锁。更新任务时带上当前 ETag，冲突会返回 409。 */
  ifMatch?: string;
}

const DEFAULT_TIMEOUT_MS = 15_000;

export async function api<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = 'GET', body, timeoutMs = DEFAULT_TIMEOUT_MS } = options;

  // 用自己的 AbortController，同时尊重调用方传进来的 signal
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  options.signal?.addEventListener('abort', () => controller.abort(), { once: true });

  const headers: Record<string, string> = { accept: 'application/json' };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (options.idempotencyKey) headers['idempotency-key'] = options.idempotencyKey;
  if (options.ifMatch) headers['if-match'] = options.ifMatch;

  let response: Response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: controller.signal
    });
  } catch (cause) {
    // fetch 只在网络层失败时 reject。把它翻译成人能看懂的原因，
    // 而不是抛一个光秃秃的 "Failed to fetch"。
    if (controller.signal.aborted) {
      throw new ApiFailure(0, {
        code: 'timeout',
        message: `请求 ${path} 超时（${timeoutMs}ms）`,
        request_id: ''
      });
    }
    throw new ApiFailure(0, {
      code: 'network',
      message: `无法连接后端（${path}）。确认 ai-task 服务已启动。`,
      request_id: ''
    });
  } finally {
    clearTimeout(timer);
  }

  if (!response.ok) {
    throw new ApiFailure(response.status, await parseError(response));
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

async function parseError(response: Response): Promise<ApiError> {
  const requestId = response.headers.get('x-request-id') ?? '';
  try {
    const body = (await response.json()) as Partial<ApiError>;
    if (typeof body?.code === 'string' && typeof body?.message === 'string') {
      return { request_id: requestId, ...body } as ApiError;
    }
  } catch {
    // 响应体不是 JSON（网关返回的 HTML 错误页之类），走下面的兜底
  }
  return {
    code: `http_${response.status}`,
    message: `${response.status} ${response.statusText}`,
    request_id: requestId
  };
}
