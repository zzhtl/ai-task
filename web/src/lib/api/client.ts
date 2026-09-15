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

/**
 * 把 422 的 `details[]` 拆成「字段名 -> 该字段的错误」。
 *
 * 后端一次把所有字段的问题都给回来（ADR 0002 明确要求不是只报第一个），
 * 但 `describeError` 会把它们拼成一行放进页头 banner。要把错误显示在**出错的那个框**
 * 旁边，就得按 field 拆开——这个函数是 `Field` 组件的输入。
 *
 * 不是 `ApiFailure`（网络错、超时）时返回空对象：那不是某个字段的问题。
 */
export function fieldErrors(e: unknown): Record<string, string> {
  if (!(e instanceof ApiFailure)) return {};
  const out: Record<string, string> = {};
  for (const d of e.body.details ?? []) {
    // 同一个字段有多条时保留第一条：框底下只有一行的位置
    out[d.field] ??= d.message;
  }
  return out;
}

/**
 * 只吞掉"这个角色本来就读不到"的错误，其余照常抛。
 *
 * 之前这些地方一律是 `.catch(() => {})`。意图是对的——operator 读不到主机列表，
 * 界面退化成显示短 id 就行，不该弹个红条。但那一行同时吞掉了 500、超时和断网，
 * 于是"后端挂了"和"你没权限"在界面上长得一模一样：面板空着，没有任何解释。
 */
export function ignoreForbidden(e: unknown): void {
  if (e instanceof ApiFailure && (e.status === 403 || e.status === 401)) return;
  throw e;
}

/**
 * 会话过期时叫谁。
 *
 * 不在这里直接改 session：`session.svelte.ts` 依赖本模块，反过来 import 就成环了。
 * 留一个插槽让它自己注册，方向保持单向。
 */
let onExpired: (() => void) | null = null;

export function setUnauthorizedHandler(handler: () => void): void {
  onExpired = handler;
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
  /**
   * 拿响应头。
   *
   * 加这个是因为"要读 ETag"曾经是绕开 `api()` 去裸 fetch 的唯一理由——
   * 而绕开之后顺带丢掉了超时、结构化错误和 401 处理。
   */
  onHeaders?: (headers: Headers) => void;
  /**
   * 这个请求的 401 是**正常答案**，不是会话过期。
   * 只有登录/探测类调用该设它——别处设了就等于把过期检测关掉了。
   */
  expected401?: boolean;
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

  // 会话中途过期时，之前每个请求各自抛 401，而 LoginGate 只在挂载时查过一次
  // ——于是界面停在一个哪个接口都 401 的壳里，不硬刷新永远回不到登录页。
  //
  // `expected401` 是必要的：登录探测本身就靠 401 来分辨"该登录了"和"没开认证"，
  // 把它也当成过期处理会在登录页上打出一个死循环。
  if (response.status === 401 && !options.expected401) {
    onExpired?.();
  }
  if (!response.ok) {
    throw new ApiFailure(response.status, await parseError(response));
  }
  if (response.status === 204) return undefined as T;
  const parsed = (await response.json()) as T;
  options.onHeaders?.(response.headers);
  return parsed;
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
