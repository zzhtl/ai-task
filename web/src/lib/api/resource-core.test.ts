// 共享数据层的行为。每条断言都对应一个之前真实存在的毛病。

import { describe, expect, test } from 'bun:test';
import { ResourceCache } from './resource-core';

/** 可控时钟 + 可控可见性，免得测试里真去等。 */
function harness(visible = { value: true }) {
  let now = 1_000;
  const cache = new ResourceCache({ now: () => now, visible: () => visible.value });
  return { cache, advance: (ms: number) => (now += ms), at: () => now };
}

const deferred = <T>() => {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

describe('去重与合并', () => {
  test('同一个 key 的并发订阅只发一个请求', async () => {
    const { cache } = harness();
    let calls = 0;
    const fetcher = async () => {
      calls += 1;
      return 'x';
    };
    cache.subscribe('tasks', fetcher, () => {});
    cache.subscribe('tasks', fetcher, () => {});
    await Promise.all([cache.refresh('tasks'), cache.refresh('tasks')]);
    // 之前：五个页面各自拉一遍 listTasks()
    expect(calls).toBe(1);
  });

  test('在途请求没回来之前不开下一轮', async () => {
    const { cache, advance } = harness();
    let calls = 0;
    const gate = deferred<string>();
    cache.subscribe(
      'slow',
      async () => {
        calls += 1;
        return gate.promise;
      },
      () => {},
      1000
    );
    void cache.refresh('slow');
    advance(5000);
    cache.tick();
    cache.tick();
    expect(calls).toBe(1);
    gate.resolve('done');
    await Promise.resolve();
  });
});

describe('缓存', () => {
  test('切页面回来先给旧数据，不白屏', async () => {
    const { cache } = harness();
    const unsub = cache.subscribe('hosts', async () => ['a'], () => {});
    await cache.refresh('hosts');
    unsub(); // 离开页面

    // 再进来：数据还在
    expect(cache.read<string[]>('hosts').data).toEqual(['a']);
  });

  test('ttl 内不重复发请求', async () => {
    const { cache, advance } = harness();
    let calls = 0;
    cache.subscribe('t', async () => ++calls, () => {});
    await cache.refresh('t', 5000);
    await cache.refresh('t', 5000);
    expect(calls).toBe(1);
    advance(6000);
    await cache.refresh('t', 5000);
    expect(calls).toBe(2);
  });

  test('失败时保留上一次的数据', async () => {
    const { cache } = harness();
    let ok = true;
    cache.subscribe(
      'flaky',
      async () => {
        if (!ok) throw new Error('boom');
        return 'good';
      },
      () => {}
    );
    await cache.refresh('flaky');
    ok = false;
    await cache.refresh('flaky');
    const entry = cache.read<string>('flaky');
    expect(entry.data).toBe('good');
    expect((entry.error as Error).message).toBe('boom');
  });
});

describe('轮询节流', () => {
  test('标签页不可见时一次都不拉', async () => {
    const visible = { value: true };
    const { cache, advance } = harness(visible);
    let calls = 0;
    cache.subscribe('live', async () => ++calls, () => {}, 1000);
    await cache.refresh('live');
    expect(calls).toBe(1);

    visible.value = false;
    advance(10_000);
    cache.tick();
    cache.tick();
    // 浏览器只把 setInterval 节流到 1 秒，对 3～10 秒的轮询等于没节流
    expect(calls).toBe(1);

    visible.value = true;
    cache.resume();
    await Promise.resolve();
    expect(calls).toBe(2);
  });

  test('多个订阅者取最短的那个间隔', async () => {
    const { cache, advance } = harness();
    let calls = 0;
    const f = async () => ++calls;
    const slow = cache.subscribe('a', f, () => {}, 10_000);
    cache.subscribe('a', f, () => {}, 1000);
    await cache.refresh('a');

    advance(1500);
    cache.tick();
    await Promise.resolve();
    expect(calls).toBe(2);

    // 要 1 秒的那个走了，就该退回 10 秒
    slow();
  });

  test('没有订阅者就不轮询', async () => {
    const { cache, advance } = harness();
    let calls = 0;
    const unsub = cache.subscribe('z', async () => ++calls, () => {}, 1000);
    await cache.refresh('z');
    unsub();
    advance(10_000);
    cache.tick();
    expect(calls).toBe(1);
  });
});

describe('失效', () => {
  test('按前缀失效并立刻重拉', async () => {
    const { cache } = harness();
    let runs = 0;
    let tasks = 0;
    cache.subscribe('runs', async () => ++runs, () => {});
    cache.subscribe('runs:abc', async () => ++runs, () => {});
    cache.subscribe('tasks', async () => ++tasks, () => {});
    await Promise.all([cache.refresh('runs'), cache.refresh('runs:abc'), cache.refresh('tasks')]);

    // SSE 收到 run_finished 时调的就是它
    cache.invalidate('runs');
    await Promise.resolve();
    await Promise.resolve();
    expect(runs).toBe(4);
    expect(tasks).toBe(1);
  });
});

describe('取消', () => {
  test('取消之后迟到的响应不会覆盖缓存', async () => {
    const { cache } = harness();
    const gate = deferred<string>();
    cache.subscribe('nav', async () => gate.promise, () => {});
    const inflight = cache.refresh('nav');
    cache.cancel('nav');
    gate.resolve('迟到的数据');
    await inflight;
    // 请求带着 signal 发出去，取消之后结果一律丢弃
    expect(cache.read<string>('nav').data).toBeUndefined();
  });

  test('signal 真的传给了 fetcher', async () => {
    const { cache } = harness();
    let seen: AbortSignal | null = null;
    cache.subscribe(
      's',
      async (signal) => {
        seen = signal;
        return 1;
      },
      () => {}
    );
    await cache.refresh('s');
    expect(seen).not.toBeNull();
    expect(seen!.aborted).toBe(false);
  });
});

describe('取消之后还能恢复', () => {
  test('取消掉的在途请求不会被下一次 refresh 复用', async () => {
    // 这是实测撞到过的一次：组件卸载时取消、紧接着重挂，
    // 新的 refresh 拿到那个已经 abort 的 promise，.then 里 signal.aborted 为真、
    // 什么都不写——缓存永远是空的，页面卡在骨架屏上，而且不报错。
    const { cache } = harness();
    let calls = 0;
    const gates: Array<(v: string) => void> = [];
    cache.subscribe(
      'k',
      async () => {
        calls += 1;
        const d = deferred<string>();
        gates.push(d.resolve);
        return d.promise;
      },
      () => {}
    );

    const first = cache.refresh('k');
    cache.cancel('k');
    const second = cache.refresh('k');
    expect(calls).toBe(2);

    gates[0]?.('被丢掉的');
    gates[1]?.('真正要的');
    await Promise.all([first, second]);

    expect(cache.read<string>('k').data).toBe('真正要的');
  });
});
