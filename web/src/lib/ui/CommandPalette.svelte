<script lang="ts">
  /**
   * ⌘K 命令面板。
   *
   * 这个系统里最常做的事是"找到某个 run / 某个任务"，而它们的名字是人起的、
   * id 是 UUID。靠点导航要三四次点击，靠搜索是一次。
   *
   * 每次打开面板拉一次最新的任务和 run，之后只在本地过滤。
   */
  import { api } from '$api/client';
  import { listAllTasks } from '$api/runs';
  import type { Page } from '$api/types/Page';
  import type { RunSummary } from '$api/types/RunSummary';
  import { theme } from './theme.svelte';

  let { onclose, onnavigate }: { onclose: () => void; onnavigate: (href: string) => void } =
    $props();

  interface Item {
    /** 跳转类条目的目标。动作类条目没有它，走 `run`。 */
    href?: string;
    run?: () => void;
    label: string;
    hint?: string;
    kind: string;
  }

  const STATIC: Item[] = [
    { href: '/', label: '概览', kind: '导航' },
    { href: '/tasks', label: '任务', kind: '导航' },
    { href: '/tasks/new', label: '新建任务', hint: '写编排、选主机', kind: '动作' },
    { href: '/runs', label: '执行记录', kind: '导航' },
    { href: '/approvals', label: '待审批', kind: '导航' },
    { href: '/hosts', label: '主机', hint: '加 SSH 机器', kind: '导航' },
    { href: '/rules', label: '规则与策略', kind: '导航' },
    { href: '/users', label: '用户', kind: '导航' },
    { href: '/audit', label: '审计', hint: '谁做了什么', kind: '导航' },
    {
      run: () => theme.toggle(),
      label: '切换深色 / 浅色',
      kind: '动作'
    }
  ];

  const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

  let query = $state('');
  let dynamic = $state<Item[]>([]);
  let cursor = $state(0);
  let input = $state<HTMLInputElement | null>(null);
  let el = $state<HTMLDialogElement | null>(null);

  // showModal() 才有焦点收拢、::backdrop 和 Esc；之前这里是个 div 拼的"模态"，
  // Tab 能一路跑到背后的页面上去。
  $effect(() => {
    if (el && !el.open) el.showModal();
  });

  $effect(() => {
    input?.focus();
  });

  // 打开时拉一次。之前是每敲一个键就重拉两个接口——既浪费，响应乱序时还会
  // 显示上一个关键词的结果。任务和 run 在面板开着的这几秒里变不了多少。
  let taskIds = new Set<string>();
  $effect(() => {
    let cancelled = false;
    Promise.all([
      listAllTasks().catch(() => []),
      // 用 ts-rs 生成的类型，不是内联的匿名结构：后端改了字段这里才会编译报错
      api<Page<RunSummary>>('/api/v1/runs?limit=20').catch(() => ({ items: [] as RunSummary[] }))
    ]).then(([tasks, runs]) => {
      if (cancelled) return;
      taskIds = new Set(tasks.map((t) => t.id));
      const names = new Map(tasks.map((t) => [t.id, t.name]));
      dynamic = [
        ...tasks.map((t) => ({
          href: `/tasks/${t.id}`,
          label: t.name,
          hint: t.id.slice(0, 8),
          kind: '任务'
        })),
        ...runs.items.map((r) => ({
          href: `/runs/${r.id}`,
          label: `${names.get(r.task_id) ?? 'run'} · ${r.id.slice(0, 8)}`,
          hint: r.status,
          kind: '执行'
        }))
      ];
    });
    return () => {
      cancelled = true;
    };
  });

  const results = $derived.by(() => {
    const all = [...STATIC, ...dynamic];
    const q = query.trim().toLowerCase();
    if (!q) return all.slice(0, 12);
    // 贴进来一个完整的 id：本地列表里只有最近 20 次执行，更早的只能按 id 直接去。
    // 认得出是任务就先给任务，否则先给执行记录
    if (UUID.test(q)) {
      const direct: Item[] = [
        { href: `/runs/${q}`, label: `打开执行记录 ${q.slice(0, 8)}`, kind: '执行' },
        { href: `/tasks/${q}`, label: `打开任务 ${q.slice(0, 8)}`, kind: '任务' }
      ];
      return taskIds.has(q) ? direct.reverse() : direct;
    }
    return all
      .filter((i) => `${i.label} ${i.hint ?? ''} ${i.kind}`.toLowerCase().includes(q))
      .slice(0, 12);
  });

  function choose(item: Item) {
    if (item.href) onnavigate(item.href);
    else {
      item.run?.();
      onclose();
    }
  }

  $effect(() => {
    // 结果变了就把光标拉回顶部，免得停在一个已经不存在的行上
    void results;
    cursor = 0;
  });

  // Escape 不在这里处理：原生 <dialog> 自己会关，然后派发 close 事件。
  function onKey(event: KeyboardEvent) {
    if (event.key === 'ArrowDown' || (event.ctrlKey && event.key === 'n')) {
      event.preventDefault();
      cursor = (cursor + 1) % Math.max(results.length, 1);
    } else if (event.key === 'ArrowUp' || (event.ctrlKey && event.key === 'p')) {
      event.preventDefault();
      cursor = (cursor - 1 + results.length) % Math.max(results.length, 1);
    } else if (event.key === 'Enter' && results[cursor]) {
      event.preventDefault();
      choose(results[cursor]);
    }
  }
</script>

<!-- 点 ::backdrop 时 e.target 就是 dialog 本身，这是原生 dialog 认背景点击的标准写法 -->
<dialog
  bind:this={el}
  class="palette"
  aria-label="命令面板"
  onclose={() => onclose()}
  onclick={(event) => {
    if (event.target === el) onclose();
  }}
>
  <input
    bind:this={input}
    bind:value={query}
    onkeydown={onKey}
    placeholder="跳转到任务、执行记录，或直接输入动作…"
    spellcheck="false"
    role="combobox"
    aria-expanded="true"
    aria-controls="cmdk-results"
    aria-activedescendant={results[cursor] ? `cmdk-${cursor}` : undefined}
  />
  <!-- 焦点始终留在输入框里，选中项靠 aria-activedescendant 播报 -->
  <ul id="cmdk-results" role="listbox" aria-label="搜索结果">
    {#each results as item, i (item.kind + (item.href ?? '') + item.label)}
      <li id="cmdk-{i}" role="option" aria-selected={i === cursor}>
        <button
          tabindex="-1"
          class:on={i === cursor}
          onmouseenter={() => (cursor = i)}
          onclick={() => choose(item)}
        >
          <span class="kind">{item.kind}</span>
          <span class="label">{item.label}</span>
          {#if item.hint}<span class="hint mono">{item.hint}</span>{/if}
        </button>
      </li>
    {:else}
      <li class="none">没有匹配的东西</li>
    {/each}
  </ul>
  <footer>
    <kbd>↑</kbd><kbd>↓</kbd> 选择　<kbd>↵</kbd> 打开　<kbd>esc</kbd> 关闭
  </footer>
</dialog>

<style>
  .palette::backdrop {
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(2px);
  }
  .palette {
    position: fixed;
    top: 12vh;
    left: 50%;
    transform: translateX(-50%);
    margin: 0;
    padding: 0;
    max-width: none;
    max-height: none;
    width: min(560px, 92vw);
    color: var(--fg);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--r3);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
    animation: palette-in var(--dur-2) var(--ease-out);
  }
  /* 这个不能并进全局 rise：居中靠 translateX(-50%)，动画里得一起带着 */
  @keyframes palette-in {
    from {
      opacity: 0;
      transform: translateX(-50%) translateY(-6px);
    }
  }
  input {
    width: 100%;
    border: none;
    border-bottom: 1px solid var(--line);
    border-radius: 0;
    background: transparent;
    padding: var(--s3) var(--s4);
    font-size: var(--t-md);
  }
  input:focus-visible {
    outline: none;
    box-shadow: none;
    border-color: var(--line);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: var(--s1);
    max-height: 46vh;
    overflow-y: auto;
  }
  ul button {
    display: flex;
    align-items: baseline;
    gap: var(--s3);
    width: 100%;
    padding: 0.4rem var(--s3);
    border: none;
    background: transparent;
    border-radius: var(--r2);
    text-align: left;
  }
  ul button.on {
    background: var(--surface-3);
  }
  .kind {
    flex: 0 0 2.6rem;
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }
  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hint {
    color: var(--fg-faint);
    font-size: var(--t-xs);
  }
  .none {
    padding: var(--s4);
    color: var(--fg-faint);
    font-size: var(--t-base);
  }
  footer {
    border-top: 1px solid var(--line);
    padding: var(--s2) var(--s4);
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  kbd {
    font-family: var(--mono);
    padding: 0.05rem 0.28rem;
    border: 1px solid var(--line);
    border-radius: var(--r1);
    margin-right: 2px;
  }
</style>
