<script lang="ts">
  /**
   * 在多台机器上各跑一次的结果：一台一行，失败的排在前面——批量操作要先看哪几台出了问题。
   *
   * 输出是服务端截好的末尾 4 KiB。默认只露第一行，点开看全文。
   */
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import { humanDuration } from '$lib/ui/format';
  import type { HostsBlock } from './process';

  let { block }: { block: HostsBlock } = $props();

  let open = $state<Record<string, boolean>>({});

  const RANK = { failed: 0, pending: 1, succeeded: 2 } as const;
  const rows = $derived(
    block.targets
      .map((t) => {
        const result = block.results[t.hostId] ?? null;
        const state = result === null ? 'pending' : result.ok ? 'succeeded' : 'failed';
        return { ...t, result, state: state as keyof typeof RANK };
      })
      .sort((a, b) => RANK[a.state] - RANK[b.state])
  );
  const count = (state: keyof typeof RANK) => rows.filter((r) => r.state === state).length;

  function firstLine(r: NonNullable<(typeof rows)[number]['result']>): string {
    const text = r.error ?? (r.stdout || r.stderr);
    return text.split('\n').find((l) => l.trim() !== '')?.trim() ?? '';
  }
</script>

<div class="host-results">
  <p class="counts">
    在 {block.targets.length} 台机器上各跑一次：
    <span class="ok-count">成功 {count('succeeded')}</span>
    {#if count('failed')}<span class="bad-count">失败 {count('failed')}</span>{/if}
    {#if count('pending')}<span class="faint">{block.ended ? '没跑完' : '进行中'} {count('pending')}</span>{/if}
  </p>
  <table>
    <thead>
      <tr><th>主机</th><th>结果</th><th class="num">退出码</th><th class="num">耗时</th><th>输出</th></tr>
    </thead>
    <tbody>
      {#each rows as row (row.hostId)}
        {@const r = row.result}
        <tr>
          <td class="nowrap name">{row.name}</td>
          <td class="nowrap">
            <StatusBadge
              status={row.state === 'pending' ? (block.ended ? 'interrupted' : 'running') : row.state}
              variant="text"
            />
            {#if r && r.attempt > 1}<span class="faint small">第 {r.attempt} 次</span>{/if}
          </td>
          <td class="num">{r?.exitCode ?? '—'}</td>
          <td class="num nowrap">{r && r.durationMs !== null ? humanDuration(r.durationMs) : '—'}</td>
          <td>
            {#if r}
              <div class="out">
                <span class="line ellipsis" class:bad-count={!r.ok}>{firstLine(r) || '（没有输出）'}</span>
                {#if r.stdout || r.stderr}
                  <button
                    class="btn-ghost btn-sm"
                    aria-expanded={!!open[row.hostId]}
                    onclick={() => (open[row.hostId] = !open[row.hostId])}
                  >
                    {open[row.hostId] ? '收起' : '全部输出'}
                  </button>
                {/if}
              </div>
            {/if}
          </td>
        </tr>
        {#if r && open[row.hostId]}
          <tr class="full">
            <td colspan="5">
              {#if r.stdout}<pre class="mono">{r.stdout}</pre>{/if}
              {#if r.stderr}<pre class="mono err-out">{r.stderr}</pre>{/if}
              {#if r.cgroupMode && r.cgroupMode !== 'systemd'}
                <p class="faint small">这台机器上资源上限没生效，只能按 {r.cgroupMode} 记账。</p>
              {/if}
            </td>
          </tr>
        {/if}
      {/each}
    </tbody>
  </table>
</div>

<style>
  .host-results {
    margin: var(--s2) 0;
    border: 1px solid var(--line);
    border-radius: var(--r2);
    overflow: auto;
  }
  .counts {
    display: flex;
    gap: var(--s3);
    margin: 0;
    padding: var(--s2) var(--s3);
    background: var(--surface-2);
    font-size: var(--t-sm);
  }
  .ok-count {
    color: var(--ok-fg);
  }
  .bad-count {
    color: var(--bad-fg);
  }
  table {
    width: 100%;
  }
  th:first-child,
  td:first-child {
    padding-left: var(--s3);
  }
  .name {
    font-weight: 500;
  }
  .out {
    display: flex;
    align-items: center;
    gap: var(--s2);
    max-width: 60ch;
  }
  .line {
    min-width: 0;
    font-size: var(--t-sm);
  }
  .full td {
    background: var(--surface-2);
  }
  pre {
    margin: 0 0 var(--s2);
    max-height: 20rem;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--t-xs);
  }
  .err-out {
    color: var(--bad-fg);
  }
</style>
