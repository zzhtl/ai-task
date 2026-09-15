// 把 ResourceCache 接到 Svelte 的响应式上。
//
// 这一层刻意很薄：逻辑全在 resource-core.ts 里（纯函数、可 bun test），
// 这里只负责"缓存变了就让组件重渲染"和全站那一个心跳。

import { ResourceCache, type Fetcher } from './resource-core';

const cache = new ResourceCache({
  visible: () => typeof document === 'undefined' || document.visibilityState === 'visible'
});

/** 全站唯一的心跳，替掉原来十个各自为政的 setInterval。 */
if (typeof window !== 'undefined') {
  setInterval(() => cache.tick(), 1000);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') cache.resume();
  });
  // 断网恢复时立刻补一次，不用干等一个轮询周期
  window.addEventListener('online', () => cache.resume());
}

export interface ResourceOptions {
  /** 轮询间隔（毫秒）。不给就只在挂载时拉一次。 */
  pollMs?: number;
  /** 这么久之内不重复拉。切页面回来时用得上。 */
  ttlMs?: number;
}

export interface Resource<T> {
  readonly data: T | undefined;
  readonly error: unknown;
  readonly loading: boolean;
  /** 从没成功拉到过。用来区分"正在首次加载"和"正在后台刷新"。 */
  readonly pending: boolean;
  refresh(): Promise<void>;
}

/**
 * 订阅一个远端资源。**必须在组件的初始化期调用**（和 `$effect` 一样的约束），
 * 卸载时自动退订。
 */
export function resource<T>(
  key: string,
  fetcher: Fetcher<T>,
  options: ResourceOptions = {}
): Resource<T> {
  // 缓存本身不是 rune，靠这个计数器把"内容变了"翻译成响应式信号
  let version = $state(0);

  $effect(() => {
    const unsubscribe = cache.subscribe(key, fetcher, () => (version += 1), options.pollMs ?? 0);
    void cache.refresh(key, options.ttlMs ?? 0);
    // **退订时不掐在途请求。**
    //
    // 一开始这里调了 cache.cancel(key)，理由是"结果没人要了"。那是错的：
    // 同一个 key 可能还有别的订阅者；而且组件卸载后立刻重挂（SPA 路由、
    // hydration）时，新的一次 refresh 会拿到那个已经被 abort 的 promise，
    // 于是 .then 里 `signal.aborted` 为真、什么都不写——缓存永远停在空的状态，
    // 页面卡在骨架屏上。请求本来就有 15 秒超时兜底，让它跑完并把结果存进缓存
    // 反而是对的：下次进来就是热的。
    return unsubscribe;
  });

  return {
    get data() {
      void version;
      return cache.read<T>(key).data;
    },
    get error() {
      void version;
      return cache.read<T>(key).error;
    },
    get loading() {
      void version;
      return cache.read<T>(key).loading;
    },
    get pending() {
      void version;
      const entry = cache.read<T>(key);
      return entry.at === 0 && entry.data === undefined;
    },
    refresh: () => cache.refresh(key)
  };
}

/**
 * 让某一类缓存过期并立刻重拉。
 *
 * **SSE 是失效信号，缓存是存储。** run 跑完了，首页和执行列表就该变——
 * 与其让它们各自缩短轮询间隔去"撞"到这个变化，不如收到事件时直接失效。
 */
export function invalidate(...prefixes: string[]): void {
  for (const prefix of prefixes) cache.invalidate(prefix);
}

/**
 * 会跟着标签页可见性停下来的定时器。
 *
 * 给那些**带游标追加**的列表用（`/runs`、任务详情的执行记录）：
 * 它们的轮询要把新数据并进已翻出来的那几页、保住滚动位置，
 * 通用缓存的"整份替换"模型表达不了这件事，硬塞进去会弄丢那个行为。
 *
 * 但"标签页看不见就别打接口"这件事和缓存无关，可以单独拿出来——
 * 浏览器只把 setInterval 节流到 1 秒，对 3～10 秒的轮询等于没节流。
 */
export function pollWhileVisible(run: () => void, everyMs: number): () => void {
  if (typeof document === 'undefined') return () => {};

  let timer: ReturnType<typeof setInterval> | null = null;
  const start = () => {
    if (timer === null) timer = setInterval(run, everyMs);
  };
  const stop = () => {
    if (timer !== null) {
      clearInterval(timer);
      timer = null;
    }
  };
  const sync = () => {
    if (document.visibilityState === 'visible') {
      // 切回来时先补一次，不用干等一个周期
      run();
      start();
    } else {
      stop();
    }
  };

  sync();
  document.addEventListener('visibilitychange', sync);
  return () => {
    stop();
    document.removeEventListener('visibilitychange', sync);
  };
}
