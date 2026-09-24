// SSE 订阅的边界行为。
//
// 这里只测一件事，但它是会把页面打崩的那件：服务端在 `Last-Event-ID` 缺失或
// 解析不了时会**从头全量重放**（sse.rs 的 `unwrap_or(0)`）。重放到达时，
// run 详情页那个 `{#each events as e (e.seq)}` 会因为 key 重复直接抛异常。

import { beforeEach, describe, expect, test } from 'bun:test';
import { subscribeRunEvents } from './events';

/** 只实现 events.ts 真正用到的那部分 EventSource。 */
class FakeEventSource {
  static last: FakeEventSource | null = null;
  static readonly CLOSED = 2;

  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(readonly url: string) {
    FakeEventSource.last = this;
  }
  close() {
    this.readyState = FakeEventSource.CLOSED;
  }
  emit(seq: number, kind = 'log') {
    this.onmessage?.({ data: JSON.stringify({ seq, node_key: null, body: { kind } }) });
  }
}

beforeEach(() => {
  FakeEventSource.last = null;
  (globalThis as Record<string, unknown>).EventSource = FakeEventSource;
});

describe('subscribeRunEvents', () => {
  test('按 seq 递增正常透传', () => {
    const seen: number[] = [];
    subscribeRunEvents('r1', { onEvent: (e) => seen.push(e.seq) });
    const src = FakeEventSource.last!;
    src.emit(1);
    src.emit(2);
    src.emit(3);
    expect(seen).toEqual([1, 2, 3]);
  });

  test('服务端全量重放时不会把同一条 seq 交付两次', () => {
    const seen: number[] = [];
    subscribeRunEvents('r1', { onEvent: (e) => seen.push(e.seq) });
    const src = FakeEventSource.last!;
    src.emit(1);
    src.emit(2);
    src.emit(3);
    // 断线重连，Last-Event-ID 丢了：服务端从 1 开始重放
    src.emit(1);
    src.emit(2);
    src.emit(3);
    src.emit(4);
    // 下游按 seq 做 key，重复一次就是 each_key_duplicate
    expect(seen).toEqual([1, 2, 3, 4]);
    expect(new Set(seen).size).toBe(seen.length);
  });

  test('坏掉的一条不会打断整条流', () => {
    const seen: number[] = [];
    subscribeRunEvents('r1', { onEvent: (e) => seen.push(e.seq) });
    const src = FakeEventSource.last!;
    src.emit(1);
    src.onmessage?.({ data: '{ 这不是 JSON' });
    src.emit(2);
    expect(seen).toEqual([1, 2]);
  });

  test('收到终态就主动关流，不让 EventSource 当成断线去重连', () => {
    const states: string[] = [];
    subscribeRunEvents('r1', {
      onEvent: () => {},
      onStatus: (s) => states.push(s)
    });
    const src = FakeEventSource.last!;
    src.emit(1, 'run_finished');
    expect(src.readyState).toBe(FakeEventSource.CLOSED);
    expect(states.at(-1)).toBe('closed');
  });
});

describe('连接被服务端拒绝之后', () => {
  test('退避后自己新建连接，并从断点续传', () => {
    const seen: number[] = [];
    const states: string[] = [];
    // 用假计时器太重；这里直接把 setTimeout 换成立即执行的版本
    const realSetTimeout = globalThis.setTimeout;
    (globalThis as Record<string, unknown>).setTimeout = ((fn: () => void) => {
      fn();
      return 0;
    }) as unknown as typeof setTimeout;
    try {
      subscribeRunEvents('r1', { onEvent: (e) => seen.push(e.seq), onStatus: (s) => states.push(s) });
      const first = FakeEventSource.last!;
      first.emit(1);
      first.emit(2);
      // 服务端给了 409（同时打开的流太多）：EventSource 把连接判死
      first.readyState = FakeEventSource.CLOSED;
      first.onerror?.();

      const second = FakeEventSource.last!;
      expect(second).not.toBe(first);
      // 不从头重放：带上已经收到的最后一条
      expect(second.url).toBe('/api/v1/runs/r1/events?after=2');
      second.emit(3);
      expect(seen).toEqual([1, 2, 3]);
      expect(states).toContain('retrying');
    } finally {
      (globalThis as Record<string, unknown>).setTimeout = realSetTimeout;
    }
  });

  test('一直连不上时，重试有上限，最后标成已断开', () => {
    const states: string[] = [];
    const realSetTimeout = globalThis.setTimeout;
    (globalThis as Record<string, unknown>).setTimeout = ((fn: () => void) => {
      fn();
      return 0;
    }) as unknown as typeof setTimeout;
    try {
      subscribeRunEvents('r1', { onEvent: () => {}, onStatus: (s) => states.push(s) });
      for (let i = 0; i < 10; i++) {
        const src = FakeEventSource.last!;
        if (src.readyState === FakeEventSource.CLOSED && states.at(-1) === 'closed') break;
        src.readyState = FakeEventSource.CLOSED;
        src.onerror?.();
      }
      expect(states.at(-1)).toBe('closed');
    } finally {
      (globalThis as Record<string, unknown>).setTimeout = realSetTimeout;
    }
  });
});
