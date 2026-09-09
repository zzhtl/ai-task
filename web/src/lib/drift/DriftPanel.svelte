<script lang="ts">
  /**
   * 漂移报告。
   *
   * 回答的是同一个问题：**输入没变、输出变了吗。**
   * 输入变了而输出跟着变，那是改动生效了，要显示但不该告警。
   */
  import { api } from '$api/client';

  interface Change {
    path: string;
    before: unknown;
    after: unknown;
  }
  interface Report {
    run_id: string;
    baseline_run_id: string | null;
    no_baseline_reason?: string;
    same_fingerprint: boolean;
    same_digest: boolean;
    alarming: boolean;
    changes: Change[];
    dry_run: boolean;
  }

  let { runId, revision = 0 }: { runId: string; revision?: number } = $props();

  let report = $state<Report | null>(null);

  $effect(() => {
    void revision;
    if (!runId) return;
    api<Report>(`/api/v1/runs/${runId}/drift`)
      .then((r) => (report = r))
      .catch(() => (report = null));
  });

  function show(value: unknown): string {
    return value === undefined || value === null ? '（无）' : JSON.stringify(value);
  }
</script>

{#if report}
  <section class="drift" class:alarming={report.alarming}>
    <h2>
      与基线比较
      {#if report.dry_run}<span class="shadow">影子执行</span>{/if}
    </h2>

    {#if !report.baseline_run_id}
      <p class="muted">{report.no_baseline_reason}</p>
    {:else}
      <p class="verdict">
        {#if report.alarming}
          <strong class="bad">行为漂移</strong>
          输入条件没变，输出却变了。
        {:else if !report.same_fingerprint}
          <strong>输入条件已变</strong>
          输出跟着变是预期内的——这是改动生效了，不是漂移。
        {:else if report.same_digest}
          <strong class="ok">无漂移</strong>
          输入条件与输出摘要都与基线一致。
        {:else}
          <strong>输出有差异</strong>
        {/if}
        <a href="/runs/{report.baseline_run_id}" class="muted">
          基线 {report.baseline_run_id.slice(0, 8)}
        </a>
      </p>

      {#if report.changes.length}
        <table>
          <thead>
            <tr><th>字段</th><th>基线</th><th>本次</th></tr>
          </thead>
          <tbody>
            {#each report.changes as change (change.path)}
              <tr>
                <td class="mono">{change.path}</td>
                <td class="mono before">{show(change.before)}</td>
                <td class="mono after">{show(change.after)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {:else if !report.same_digest}
        <!-- 摘要不同却列不出差异：说明差异全落在被剔除的自由文本上 -->
        <p class="muted">摘要不同，但结构化字段没有差异。</p>
      {/if}
    {/if}
  </section>
{/if}

<style>
  .drift {
    margin-top: 1.25rem;
    border: 1px solid var(--line);
    border-radius: var(--r3);
    padding: 0.6rem 0.9rem;
    background: var(--surface-1);
  }
  .drift.alarming { border-color: var(--bad); }
  h2 { font-size: 0.95rem; margin: 0 0 0.5rem; }
  .shadow {
    margin-left: 0.5rem;
    font-weight: 400;
    font-size: 0.78rem;
    color: var(--fg-dim);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 0 0.5rem;
  }
  .verdict { margin: 0 0 0.5rem; font-size: 0.88rem; }
  .verdict a { margin-left: 0.5rem; }
  table { width: 100%; border-collapse: collapse; font-size: 0.82rem; }
  th { text-align: left; color: var(--fg-dim); font-weight: 500; padding: 0.2rem 0.5rem 0.2rem 0; }
  td { padding: 0.2rem 0.5rem 0.2rem 0; border-top: 1px solid var(--line); overflow-wrap: anywhere; }
  .before { color: var(--fg-dim); }
  .after { color: var(--fg); font-weight: 500; }
  .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .muted { color: var(--fg-dim); }
  .bad { color: var(--bad); }
  .ok { color: var(--ok); }
</style>
