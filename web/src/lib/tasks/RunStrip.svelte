<script lang="ts">
  /**
   * 最近几次执行，一格一次：旧的在左、新的在右，最右边那格就是"上次"。
   *
   * 颜色只是让人扫一眼就知道稳不稳；每一格的 title 和读屏名字里有文字，
   * 旁边还写着成功几次，不单靠颜色传达（WCAG 1.4.1）。
   */
  import type { RecentRun } from '$api/types/RecentRun';
  import { ago, FAILED_STATUSES } from '$lib/ui/format';
  import { successOf } from './recent';

  let { runs, slots = 10 }: { runs: RecentRun[]; slots?: number } = $props();

  const TEXT: Record<string, string> = {
    queued: '排队中',
    running: '执行中',
    succeeded: '成功',
    failed: '失败',
    cancelled: '已取消',
    timed_out: '超时',
    budget_exceeded: '预算耗尽',
    resource_exceeded: '资源击穿'
  };
  // 类名带前缀：全局样式里有同名的 .ok / .bad，撞上就会被一起染色
  const tone = (status: string) =>
    status === 'succeeded'
      ? 't-ok'
      : FAILED_STATUSES.includes(status)
        ? 't-bad'
        : status === 'running' || status === 'queued'
          ? 't-live'
          : 't-neutral';

  const cells = $derived(runs.slice(0, slots).reverse());
  const stats = $derived(successOf(runs));
  const describe = (r: RecentRun) =>
    `${TEXT[r.status] ?? r.status}${r.dry_run ? '（影子）' : ''} · ${ago(r.created_at)}`;
</script>

<span class="recent">
  <span class="strip" role="group" aria-label="最近 {runs.length} 次执行">
    {#each { length: slots - cells.length }}
      <span class="cell" aria-hidden="true"></span>
    {/each}
    {#each cells as r (r.id)}
      <a
        class="cell {tone(r.status)}"
        class:dry={r.dry_run}
        href="/runs/{r.id}"
        title={describe(r)}
        aria-label={describe(r)}
      ></a>
    {/each}
  </span>
  {#if stats.done}
    <span class="rate" class:low={stats.ok < stats.done}>成功 {stats.ok}/{stats.done}</span>
  {/if}
</span>

<style>
  .recent {
    display: inline-flex;
    align-items: center;
    gap: var(--s2);
    white-space: nowrap;
  }
  .strip {
    display: inline-flex;
    gap: 3px;
  }
  .cell {
    width: 8px;
    height: 16px;
    border-radius: 2px;
    background: var(--surface-3);
  }
  a.cell:hover,
  a.cell:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: 1px;
  }
  .cell.t-ok {
    background: var(--st-succeeded);
  }
  .cell.t-bad {
    background: var(--st-failed);
  }
  .cell.t-live {
    background: var(--st-running);
  }
  .cell.t-neutral {
    background: var(--st-cancelled);
  }
  /* 影子执行画成空心：是在试提示词，不是任务真的跑了一次 */
  .cell.dry {
    background: transparent;
    box-shadow: inset 0 0 0 1.5px var(--st-succeeded);
  }
  .cell.dry.t-bad {
    box-shadow: inset 0 0 0 1.5px var(--st-failed);
  }
  .cell.dry.t-live {
    box-shadow: inset 0 0 0 1.5px var(--st-running);
  }
  .cell.dry.t-neutral {
    box-shadow: inset 0 0 0 1.5px var(--st-cancelled);
  }
  .rate {
    font-size: var(--t-xs);
    color: var(--fg-faint);
    font-variant-numeric: tabular-nums;
  }
  .rate.low {
    color: var(--bad-fg);
  }
</style>
