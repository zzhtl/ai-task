// 共享数据层的纯逻辑部分。
//
// 之前每个页面各拿一份 `$state<T[]>([])` 自己 setInterval 拉：`listTasks()` 被五处
// 各拉一遍、`listHosts()` 四处、`listApprovals()` 四处；切页面从零重来（先白屏再骨架）；
// 十个定时器在标签页隐藏时照样跑。闲置的首页一分钟打出去约一百个请求。
//
// 这里只管缓存、去重、节流和取消，**不碰 rune**——所以能直接 bun test。
// 响应式那一层薄薄地包在 resource.svelte.ts 里，和 process.ts / Process.svelte 一个分工。

/** 一次取数。`signal` 必须透传下去，否则"取消"只是不再理会结果，请求照样在飞。 */
export type Fetcher<T> = (signal: AbortSignal) => Promise<T>;

export interface Entry<T> {
  data: T | undefined;
  error: unknown;
  /** 有没有请求在飞。用来挡住重叠轮询。 */
  loading: boolean;
  /** 上一次成功的时间戳；0 表示从没成功过。 */
  at: number;
}

interface Slot<T> {
  entry: Entry<T>;
  fetcher: Fetcher<T>;
  /** 在途的那一次。并发调用者共享它，而不是各发一个请求。 */
  inflight: Promise<void> | null;
  abort: AbortController | null;
  /** 轮询间隔，0 表示不轮询。取所有订阅者里最短的那个。 */
  pollMs: number;
  /** 下次该拉的时间戳。 */
  nextAt: number;
  subscribers: Set<() => void>;
  /** 每个订阅者要的间隔，退订时要能把 pollMs 重算回去。 */
  wanted: Map<symbol, number>;
}

export interface CacheOptions {
  /** 现在几点。测试里注进去，生产用 Date.now。 */
  now?: () => number;
  /** 标签页可见吗。不可见时不轮询。 */
  visible?: () => boolean;
}

export class ResourceCache {
  private slots = new Map<string, Slot<unknown>>();
  private now: () => number;
  private visible: () => boolean;

  constructor(options: CacheOptions = {}) {
    this.now = options.now ?? (() => Date.now());
    this.visible = options.visible ?? (() => true);
  }

  /** 当前缓存内容。没有就返回一个空壳，调用方不用判空。 */
  read<T>(key: string): Entry<T> {
    const slot = this.slots.get(key) as Slot<T> | undefined;
    return slot?.entry ?? { data: undefined, error: undefined, loading: false, at: 0 };
  }

  /**
   * 订阅一个 key。返回退订函数。
   *
   * 退订只停轮询，**不清缓存**——切回来的时候先显示上次的数据再后台刷新，
   * 比"白屏 → 骨架 → 数据"要好，而且那份数据通常还是对的。
   */
  subscribe<T>(
    key: string,
    fetcher: Fetcher<T>,
    onChange: () => void,
    pollMs = 0
  ): () => void {
    let slot = this.slots.get(key) as Slot<T> | undefined;
    if (!slot) {
      slot = {
        entry: { data: undefined, error: undefined, loading: false, at: 0 },
        fetcher,
        inflight: null,
        abort: null,
        pollMs: 0,
        nextAt: 0,
        subscribers: new Set(),
        wanted: new Map()
      };
      this.slots.set(key, slot as Slot<unknown>);
    }
    // fetcher 可能闭包了新的参数（比如换了 taskId），以最后一次订阅的为准
    slot.fetcher = fetcher;
    slot.subscribers.add(onChange);

    const token = Symbol('subscriber');
    if (pollMs > 0) slot.wanted.set(token, pollMs);
    this.retune(slot);

    return () => {
      slot.subscribers.delete(onChange);
      slot.wanted.delete(token);
      this.retune(slot);
    };
  }

  /** 取所有订阅者里最短的间隔：有人要 3 秒，就不能按 10 秒走。 */
  private retune(slot: Slot<unknown>): void {
    const wants = [...slot.wanted.values()];
    slot.pollMs = wants.length ? Math.min(...wants) : 0;
    if (slot.pollMs > 0 && slot.nextAt === 0) slot.nextAt = this.now();
  }

  /**
   * 拉一次。已经有在途请求时**返回同一个 promise**，不再发第二个。
   *
   * `force` 为 false 且缓存还在 `ttlMs` 内时直接返回，不发请求。
   */
  refresh(key: string, ttlMs = 0): Promise<void> {
    const slot = this.slots.get(key);
    if (!slot) return Promise.resolve();
    // 复用在途请求——**但被取消的那个不算**。
    // 被 abort 的 promise 结算时什么都不写，跟着它等只会等来一个空缓存；
    // 这正是"卸载时取消、紧接着重挂"会把页面永久卡在骨架屏上的原因。
    if (slot.inflight && !slot.abort?.signal.aborted) return slot.inflight;
    if (ttlMs > 0 && slot.entry.at > 0 && this.now() - slot.entry.at < ttlMs) {
      return Promise.resolve();
    }

    const abort = new AbortController();
    slot.abort = abort;
    slot.entry.loading = true;
    this.emit(slot);

    const run = slot
      .fetcher(abort.signal)
      .then((data) => {
        if (abort.signal.aborted) return;
        slot.entry.data = data;
        slot.entry.error = undefined;
        slot.entry.at = this.now();
      })
      .catch((error: unknown) => {
        if (abort.signal.aborted) return;
        // 之前失败过但现在有旧数据时，保留旧数据：显示"有点旧"比显示空白有用
        slot.entry.error = error;
      })
      .finally(() => {
        if (slot.abort === abort) slot.abort = null;
        slot.inflight = null;
        slot.entry.loading = false;
        slot.nextAt = this.now() + slot.pollMs;
        this.emit(slot);
      });

    slot.inflight = run;
    return run;
  }

  /** 把缓存标记为过期，下一次 tick 立刻重拉。SSE 收到终态时调它。 */
  invalidate(prefix: string): void {
    for (const [key, slot] of this.slots) {
      if (key !== prefix && !key.startsWith(`${prefix}:`)) continue;
      slot.entry.at = 0;
      slot.nextAt = 0;
      if (slot.subscribers.size > 0) void this.refresh(key);
    }
  }

  /**
   * 时钟。**全站一个**，替掉原来十个各自为政的 setInterval。
   * 标签页不可见时直接返回——浏览器只把 setInterval 节流到 1 秒，
   * 对 3～10 秒的轮询几乎没有影响，得自己停。
   */
  tick(): void {
    if (!this.visible()) return;
    const now = this.now();
    for (const [key, slot] of this.slots) {
      if (slot.pollMs <= 0 || slot.subscribers.size === 0) continue;
      if (slot.inflight) continue; // 上一轮还没回来就不开下一轮
      if (now < slot.nextAt) continue;
      void this.refresh(key);
    }
  }

  /** 标签页重新可见：把已经过期的立刻补上，不等下一个间隔。 */
  resume(): void {
    const now = this.now();
    for (const [key, slot] of this.slots) {
      if (slot.subscribers.size === 0 || slot.pollMs <= 0) continue;
      if (now >= slot.nextAt) void this.refresh(key);
    }
  }

  /**
   * 取消某个 key 在途的请求。
   *
   * **只在明确要丢弃结果时用**，比如换了查询参数、旧结果已经没意义。
   * 不要拿它做"组件卸载就取消"：同一个 key 可能还有别的订阅者，
   * 而且被 abort 的 promise 会让紧接着的一次 refresh 拿到一个
   * 永远不写缓存的结果。
   */
  cancel(key: string): void {
    this.slots.get(key)?.abort?.abort();
  }

  /** 仅供测试：清空一切。 */
  reset(): void {
    for (const slot of this.slots.values()) slot.abort?.abort();
    this.slots.clear();
  }

  private emit(slot: Slot<unknown>): void {
    for (const notify of slot.subscribers) notify();
  }
}
