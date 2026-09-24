// Run 事件流的订阅封装。
//
// 用浏览器原生的 EventSource 而不是自己拿 fetch 读流：它自带断线重连，
// 并且重连时会把最后收到的 `id:` 作为 `Last-Event-ID` 头带回去——这正好对上
// 服务端按 seq 续传的语义。
//
// 但它只管**网络断了**这一种情况。服务端直接给了非 200（同时打开的流太多返回 409、
// 会话过期 401），EventSource 会把连接判死、再也不重连——界面上就一直停在
// "已断开"。这种情况由这里接手：退避后新建一条连接，用 `?after=` 从断点续传。

import type { RunEvent } from './types/RunEvent';

export type StreamStatus =
  /** 第一次连。 */
  | 'connecting'
  | 'open'
  /** 连接被拒或断了，正在退避重连。 */
  | 'retrying'
  /** 收到终态、被调用方关掉，或者重试次数用完。 */
  | 'closed';

export interface EventStreamHandlers {
  onEvent: (event: RunEvent) => void;
  /** 连接状态变化。用来在界面上区分"实时"和"已断开"。 */
  onStatus?: (status: StreamStatus) => void;
}

export interface EventStream {
  close(): void;
}

/** 退避上限。1、2、4、8、16、30 秒——大约一分钟后放弃，界面显示"已断开"。 */
const MAX_RETRIES = 6;
const MAX_BACKOFF_MS = 30_000;

/**
 * 订阅一个 run 的事件流。
 *
 * 终态事件（run_finished）到达后主动关闭：服务端也会关，但客户端先关掉能
 * 避免 EventSource 把正常结束当成断线去重连。
 */
export function subscribeRunEvents(runId: string, handlers: EventStreamHandlers): EventStream {
  let source: EventSource | null = null;
  let closed = false;
  let retries = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  // 服务端在续传位置缺失时会**从头全量重放**。真发生时，下游那个按 seq 做 key 的
  // {#each} 会直接抛 each_key_duplicate 把页面打崩。在流的边界上挡住，比让每个
  // 消费者各自记一遍强；自己新建连接时也靠它告诉服务端从哪接着发。
  let lastSeq = 0;

  const close = () => {
    if (closed) return;
    closed = true;
    if (timer) clearTimeout(timer);
    source?.close();
    handlers.onStatus?.('closed');
  };

  const open = () => {
    if (closed) return;
    const url = lastSeq > 0 ? `/api/v1/runs/${runId}/events?after=${lastSeq}` : `/api/v1/runs/${runId}/events`;
    const es = new EventSource(url);
    source = es;

    es.onopen = () => {
      retries = 0;
      handlers.onStatus?.('open');
    };

    es.onmessage = (message) => {
      let event: RunEvent;
      try {
        event = JSON.parse(message.data) as RunEvent;
      } catch {
        // 解析不了就跳过这一条。一条坏事件不该让整个流断掉。
        return;
      }
      if (event.seq <= lastSeq) return;
      lastSeq = event.seq;
      handlers.onEvent(event);
      if (event.body.kind === 'run_finished') close();
    };

    es.onerror = () => {
      if (closed) return;
      if (es.readyState !== EventSource.CLOSED) {
        // 浏览器自己在重连，会带上 Last-Event-ID，不用管
        handlers.onStatus?.('retrying');
        return;
      }
      // 连接被判死了（非 200 响应，或者服务端正常收尾但我们还没收到终态）。
      // 浏览器不会再重连，自己来。
      es.close();
      if (retries >= MAX_RETRIES) {
        close();
        return;
      }
      const delay = Math.min(MAX_BACKOFF_MS, 1000 * 2 ** retries);
      retries += 1;
      handlers.onStatus?.('retrying');
      timer = setTimeout(open, delay);
    };
  };

  handlers.onStatus?.('connecting');
  open();
  return { close };
}
