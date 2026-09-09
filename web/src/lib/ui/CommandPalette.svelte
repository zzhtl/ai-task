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
    { href: '/users', label: '用户', kind: '导航' }
  ];

  let query = $state('');
  let dynamic = $state<Item[]>([]);
  let cursor = $state(0);
  let input = $state<HTMLInputElement | null>(null);

  $effect(() => {
    input?.focus();
  });

  $effect(() => {
    // 任务和 run 一直在变，每次开面板都重拉一次
    void query;
    Promise.all([
      api<{ items: Array<{ id: string; name: string }> }>('/api/v1/tasks?limit=50').catch(() => ({
        items: []
      })),
      api<{ items: Array<{ id: string; status: string; task_id: string }> }>(
        '/api/v1/runs?limit=20'
      ).catch(() => ({ items: [] }))
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
          label: `run ${r.id.slice(0, 8)}`,
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

  function onKey(event: KeyboardEvent) {
    if (event.key === 'Escape') return onclose();
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

<!-- 点背景关掉。键盘用户走 Escape，上面已经处理 -->
<div
  class="scrim"
  role="button"
  tabindex="-1"
  aria-label="关闭命令面板"
  onclick={onclose}
  onkeydown={() => {}}
></div>

<div class="palette" role="dialog" aria-modal="true" aria-label="命令面板">
  <input
    bind:this={input}
    bind:value={query}
    onkeydown={onKey}
    placeholder="跳转到任务、执行记录，或直接输入动作…"
    spellcheck="false"
  />
  <ul>
    {#each results as item, i (item.href + item.label)}
      <li>
        <button class:on={i === cursor} onmouseenter={() => (cursor = i)} onclick={() => onnavigate(item.href)}>
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
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    backdrop-filter: blur(2px);
    z-index: var(--z-overlay);
    border: none;
    padding: 0;
  }
  .palette {
    position: fixed;
    top: 12vh;
    left: 50%;
    transform: translateX(-50%);
    width: min(560px, 92vw);
    z-index: calc(var(--z-overlay) + 1);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    border-radius: var(--r3);
    box-shadow: 0 24px 64px rgba(0, 0, 0, 0.5);
    overflow: hidden;
    animation: rise 0.13s ease-out;
  }
  @keyframes rise {
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
    font-size: 0.95rem;
  }
  input:focus-visible {
    outline: none;
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
    font-size: 0.7rem;
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
    font-size: 0.75rem;
  }
  .none {
    padding: var(--s4);
    color: var(--fg-faint);
    font-size: 0.85rem;
  }
  footer {
    border-top: 1px solid var(--line);
    padding: var(--s2) var(--s4);
    font-size: 0.72rem;
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
