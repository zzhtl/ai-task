// Run 事件流的订阅封装。
//
// 用浏览器原生的 EventSource 而不是自己拿 fetch 读流：它自带断线重连，
// 并且重连时会把最后收到的 `id:` 作为 `Last-Event-ID` 头带回去——这正好对上
// 服务端按 seq 续传的语义，不需要我们自己记状态。

import type { RunEvent } from './types/RunEvent';

export interface EventStreamHandlers {
  onEvent: (event: RunEvent) => void;
  /** 连接状态变化。用来在界面上区分"实时"和"已断开"。 */
  onStatus?: (status: 'connecting' | 'open' | 'closed') => void;
}

export interface EventStream {
  close(): void;
}

/**
 * 订阅一个 run 的事件流。
 *
 * 终态事件（run_finished）到达后主动关闭：服务端也会关，但客户端先关掉能
 * 避免 EventSource 把正常结束当成断线去重连。
 */
export function subscribeRunEvents(runId: string, handlers: EventStreamHandlers): EventStream {
  const source = new EventSource(`/api/v1/runs/${runId}/events`);
  let closed = false;
  // 服务端在 `Last-Event-ID` 缺失或解析不了时会**从头全量重放**（sse.rs 的
  // `unwrap_or(0)`）。真发生时，下游那个按 seq 做 key 的 {#each} 会直接抛
  // each_key_duplicate 把页面打崩。在流的边界上挡住，比让每个消费者各自记一遍强。
  let lastSeq = 0;

  const close = () => {
    if (closed) return;
    closed = true;
    source.close();
    handlers.onStatus?.('closed');
  };

  handlers.onStatus?.('connecting');
  source.onopen = () => handlers.onStatus?.('open');

  source.onmessage = (message) => {
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

  source.onerror = () => {
    // EventSource 在正常结束（服务端关流）时也会触发 error。已经收到终态就
    // 直接收工，否则交给它自己重连——重连会带上 Last-Event-ID，不会丢事件。
    if (source.readyState === EventSource.CLOSED) close();
    else handlers.onStatus?.('connecting');
  };

  return { close };
}
