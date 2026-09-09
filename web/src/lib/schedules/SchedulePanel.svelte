<script lang="ts">
  /**
   * 一个任务的定时配置。
   *
   * 表达式在**保存时**由后端编译一次，跑不通就 422。存进去一个算不出触发点的
   * 表达式，代价是那个任务永远不响，而且不会有任何东西报错。
   *
   * 保存后会回显接下来三次触发的**本地时间**——`0 0 * * *` 和 `0 0 * * 0`
   * 光看字符串是分不出来的，看时间就一目了然。
   */
  import { api, ApiFailure } from '$api/client';

  interface Schedule {
    id: string;
    task_id: string;
    cron: string;
    timezone: string;
    misfire: string;
    overlap: string;
    jitter_s: number;
    enabled: boolean;
    next_fire_at: string | null;
    last_fired_at: string | null;
    next_three: string[];
  }

  let { taskId }: { taskId: string } = $props();

  let items = $state<Schedule[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  let cron = $state('0 2 * * *');
  let timezone = $state('Asia/Shanghai');
  let misfire = $state('fire_once');
  let overlap = $state('skip');
  let jitter = $state(0);

  /** 常见写法。cron 的记忆负担主要在这几个上。 */
  const PRESETS = [
    { label: '每天 02:00', expr: '0 2 * * *' },
    { label: '每小时', expr: '0 * * * *' },
    { label: '每 5 分钟', expr: '*/5 * * * *' },
    { label: '工作日 09:00', expr: '0 9 * * 1-5' },
    { label: '每月 1 号 03:00', expr: '0 3 1 * *' },
    { label: '每 30 秒（6 段）', expr: '*/30 * * * * *' }
  ];

  async function load() {
    try {
      items = (await api<{ items: Schedule[] }>(`/api/v1/schedules?task_id=${taskId}`)).items;
      error = null;
    } catch (e) {
      error = describe(e);
    }
  }

  function describe(e: unknown): string {
    if (e instanceof ApiFailure) {
      const details = (e.body as { details?: Array<{ message: string }> }).details;
      return details?.length ? details.map((d) => d.message).join('；') : e.message;
    }
    return String(e);
  }

  async function act(run: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await run();
      await load();
    } catch (e) {
      error = describe(e);
    } finally {
      busy = false;
    }
  }

  const create = () =>
    act(() =>
      api('/api/v1/schedules', {
        method: 'POST',
        body: {
          task_id: taskId,
          cron,
          timezone,
          misfire,
          overlap,
          jitter_s: jitter,
          enabled: true
        }
      })
    );

  const toggle = (s: Schedule) =>
    act(() =>
      api(`/api/v1/schedules/${s.id}/enabled`, { method: 'PUT', body: { enabled: !s.enabled } })
    );

  const remove = (s: Schedule) => act(() => api(`/api/v1/schedules/${s.id}`, { method: 'DELETE' }));

  $effect(() => {
    void taskId;
    void load();
  });
</script>

<section>
  <h2>定时</h2>

  {#each items as s (s.id)}
    <article class:off={!s.enabled}>
      <div class="line">
        <code>{s.cron}</code>
        <span class="muted">{s.timezone}</span>
        {#if s.jitter_s > 0}<span class="muted">抖动 {s.jitter_s}s</span>{/if}
        <span class="muted">misfire={s.misfire} overlap={s.overlap}</span>
        <span class="spacer"></span>
        <button disabled={busy} onclick={() => toggle(s)}>{s.enabled ? '停用' : '启用'}</button>
        <button disabled={busy} onclick={() => remove(s)}>删除</button>
      </div>
      <div class="line muted">
        {#if s.enabled}
          接下来：{s.next_three.join('　·　') || '算不出触发点'}
        {:else}
          已停用（下次触发点保留：{s.next_fire_at?.replace('T', ' ').slice(0, 19) ?? '—'}）
        {/if}
        {#if s.last_fired_at}
          　上次：{s.last_fired_at.replace('T', ' ').slice(0, 19)}
        {/if}
      </div>
    </article>
  {:else}
    <p class="muted">还没有定时配置，这个任务只能手动触发。</p>
  {/each}

  {#if error}<p class="bad">{error}</p>{/if}

  <div class="new">
    <div class="presets">
      {#each PRESETS as p (p.expr)}
        <button class="chip" onclick={() => (cron = p.expr)}>{p.label}</button>
      {/each}
    </div>
    <div class="row">
      <input bind:value={cron} class="cron" placeholder="0 2 * * *" />
      <input bind:value={timezone} placeholder="Asia/Shanghai" />
      <label>
        misfire
        <select bind:value={misfire}>
          <option value="skip">skip（停服期间的都不补）</option>
          <option value="fire_once">fire_once（只补一次）</option>
          <option value="fire_all">fire_all（全部补上）</option>
        </select>
      </label>
      <label>
        overlap
        <select bind:value={overlap}>
          <option value="skip">skip（上次没跑完就跳过）</option>
          <option value="allow">allow（允许并行）</option>
          <option value="queue">queue（排队）</option>
        </select>
      </label>
      <label>
        抖动秒
        <input bind:value={jitter} type="number" min="0" max="3600" class="jitter" />
      </label>
      <button onclick={create} disabled={busy || !cron}>添加</button>
    </div>
    <p class="muted">
      支持 5 段（分 时 日 月 周）和 6 段（秒 分 时 日 月 周），以及 <code>L</code>、<code>#</code>。
      表达式在保存时就会编译一次，跑不通会当场拒绝。
    </p>
  </div>
</section>

<style>
  section { margin-top: 1.25rem; }
  h2 { font-size: 0.95rem; margin: 0 0 0.5rem; }
  article {
    border: 1px solid var(--line);
    border-radius: var(--r2);
    padding: 0.5rem 0.75rem;
    background: var(--surface-1);
    margin-bottom: 0.5rem;
  }
  article.off { opacity: 0.55; }
  .line { display: flex; gap: 0.6rem; align-items: baseline; flex-wrap: wrap; font-size: 0.85rem; }
  .spacer { flex: 1; }
  .new { border: 1px dashed var(--line); border-radius: var(--r2); padding: 0.6rem 0.75rem; }
  .presets { display: flex; gap: 0.4rem; flex-wrap: wrap; margin-bottom: 0.5rem; }
  .row { display: flex; gap: 0.5rem; align-items: flex-end; flex-wrap: wrap; }
  label { display: flex; flex-direction: column; gap: 0.2rem; font-size: 0.78rem; color: var(--fg-dim); }
  input, select, button {
    padding: 0.3rem 0.55rem;
    border: 1px solid var(--line);
    border-radius: var(--r2);
    background: var(--bg);
    color: var(--fg);
    font: inherit;
    font-size: 0.85rem;
  }
  .cron { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; min-width: 10rem; }
  .jitter { width: 5rem; }
  button { cursor: pointer; }
  button:disabled { cursor: default; opacity: 0.5; }
  .chip { font-size: 0.78rem; padding: 0.15rem 0.45rem; }
  code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .muted { color: var(--fg-dim); font-size: 0.82rem; }
  .bad { color: var(--bad); font-size: 0.85rem; }
</style>
