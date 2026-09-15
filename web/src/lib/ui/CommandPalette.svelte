<script lang="ts">
  /**
   * ⌘K 命令面板。
   *
   * 这个系统里最常做的事是"找到某个 run / 某个任务"，而它们的名字是人起的、
   * id 是 UUID。靠点导航要三四次点击，靠搜索是一次。
   *
   * 结果是**实时拉的**，不是预加载的静态列表：任务和 run 一直在变。
   */
  import { api } from '$api/client';
  import type { Page } from '$api/types/Page';
  import type { TaskSummary } from '$api/types/TaskSummary';
  import type { RunSummary } from '$api/types/RunSummary';

  let { onclose, onnavigate }: { onclose: () => void; onnavigate: (href: string) => void } =
    $props();

  interface Item {
    href: string;
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
    { href: '/audit', label: '审计', hint: '谁做了什么', kind: '导航' }
  ];

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

  $effect(() => {
    // 任务和 run 一直在变，每次开面板都重拉一次
    void query;
    Promise.all([
      // 用 ts-rs 生成的类型，不是内联的匿名结构：后端改了字段这里才会编译报错
      api<Page<TaskSummary>>('/api/v1/tasks?limit=50').catch(() => ({ items: [] })),
      api<Page<RunSummary>>('/api/v1/runs?limit=20').catch(() => ({ items: [] }))
    ]).then(([tasks, runs]) => {
      dynamic = [
        ...tasks.items.map((t) => ({
          href: `/tasks/${t.id}`,
          label: t.name,
          hint: t.id.slice(0, 8),
          kind: '任务'
        })),
        ...runs.items.map((r) => ({
          href: `/runs/${r.id}`,
          label: `${tasks.items.find((t) => t.id === r.task_id)?.name ?? 'run'} · ${r.id.slice(0, 8)}`,
          hint: r.status,
          kind: '执行'
        }))
      ];
    });
  });

  const results = $derived.by(() => {
    const all = [...STATIC, ...dynamic];
    const q = query.trim().toLowerCase();
    if (!q) return all.slice(0, 12);
    return all
      .filter((i) => `${i.label} ${i.hint ?? ''} ${i.kind}`.toLowerCase().includes(q))
      .slice(0, 12);
  });

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
      onnavigate(results[cursor].href);
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
    {#each results as item, i (item.href + item.label)}
      <li id="cmdk-{i}" role="option" aria-selected={i === cursor}>
        <button
          tabindex="-1"
          class:on={i === cursor}
          onmouseenter={() => (cursor = i)}
          onclick={() => onnavigate(item.href)}
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
