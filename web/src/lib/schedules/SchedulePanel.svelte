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
  import { session } from '$lib/auth/session.svelte';
  import { api, describeError, fieldErrors } from '$api/client';
  import { listSchedules, type Schedule } from '$api/models';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { DISPLAY_TIMEZONE, stamp } from '$lib/ui/format';
  import Icon from '$lib/ui/Icon.svelte';

  let { taskId, taskEnabled = true }: { taskId: string; taskEnabled?: boolean } = $props();

  let items = $state<Schedule[]>([]);
  const canOperate = $derived(session.can('operator'));
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let adding = $state(false);
  /** 后端 422 里按字段拆出来的错误（cron 写错是最常见的一种）。 */
  let fieldErr = $state<Record<string, string>>({});

  let cron = $state('0 2 * * *');
  let timezone = $state(DISPLAY_TIMEZONE);
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
    { label: '每 30 秒', expr: '*/30 * * * * *' }
  ];

  const MISFIRE: Record<string, string> = {
    skip: '停服期间的不补',
    fire_once: '停服期间的只补一次',
    fire_all: '停服期间的全部补上'
  };
  const OVERLAP: Record<string, string> = {
    skip: '上次没跑完就跳过',
    allow: '允许并行',
    queue: '排队等上次跑完'
  };

  async function load() {
    try {
      items = await listSchedules(taskId);
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  async function act(run: () => Promise<unknown>, done?: string) {
    busy = true;
    error = null;
    fieldErr = {};
    try {
      await run();
      await load();
      if (done) toast(done);
    } catch (e) {
      // cron 写错是这里最常见的失败，显示在表达式框底下比丢进页头有用得多
      fieldErr = fieldErrors(e);
      error = Object.keys(fieldErr).length ? null : describeError(e);
    } finally {
      busy = false;
    }
  }

  const create = () =>
    act(async () => {
      await api('/api/v1/schedules', {
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
      });
      adding = false;
    }, '定时已添加');

  const toggle = (s: Schedule) =>
    act(
      () =>
        api(`/api/v1/schedules/${s.id}/enabled`, { method: 'PUT', body: { enabled: !s.enabled } }),
      s.enabled ? '定时已停用' : '定时已启用'
    );

  let pendingDelete = $state<Schedule | null>(null);
  const remove = (s: Schedule) =>
    act(() => api(`/api/v1/schedules/${s.id}`, { method: 'DELETE' }), '定时已删除').catch(
      (e) => toastError(describeError(e))
    );

  $effect(() => {
    void taskId;
    void load();
  });
</script>

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
  title="删除这条定时？"
  danger
  confirmText="删除"
  onconfirm={() => {
    const s = pendingDelete;
    pendingDelete = null;
    if (s) void remove(s);
  }}
>
  <p><code>{pendingDelete?.cron}</code> · {pendingDelete?.timezone}</p>
  <p>删掉之后这个任务就不会在这个时间自己跑了。已经产生的执行记录不受影响。</p>
</Confirm>

<section class="card">
  <header class="card-head">
    <h2>定时</h2>
    {#if items.length}
      <span class="sub">{items.filter((s) => s.enabled).length}/{items.length} 条启用</span>
    {/if}
    <span class="spacer"></span>
    <button class="btn-sm" onclick={() => (adding = true)} disabled={!canOperate} title={canOperate ? undefined : '需要 operator 权限'}>添加定时</button>
  </header>

  {#if !taskEnabled && items.some((s) => s.enabled)}
    <p class="callout warn small">任务已停用：定时到点也<strong>不会</strong>触发。启用任务后恢复。</p>
  {/if}

  {#if !loaded}
    <Loading rows={2} />
  {:else if items.length}
    <ul class="items">
      {#each items as s (s.id)}
        <li class:off={!s.enabled}>
          <div class="line">
            <code class="cron">{s.cron}</code>
            <span class="faint">{s.timezone}</span>
            {#if s.jitter_s > 0}<span class="tag">抖动 {s.jitter_s}s</span>{/if}
            <span class="spacer"></span>
            <button class="btn-ghost btn-sm" disabled={busy || !canOperate} onclick={() => toggle(s)}>
              {s.enabled ? '停用' : '启用'}
            </button>
            <button
              class="btn-ghost btn-sm btn-icon danger"
              disabled={busy || !canOperate}
              title="删除"
              aria-label="删除定时"
              onclick={() => (pendingDelete = s)}
            >
              <Icon name="trash" />
            </button>
          </div>
          <div class="line meta">
            {#if s.enabled}
              <span title={s.next_three.join('\n')}>
                下次 <b>{s.next_three[0] ?? '算不出触发点'}</b>
              </span>
            {:else}
              <span>已停用</span>
            {/if}
            {#if s.last_fired_at}<span>上次 {stamp(s.last_fired_at)}</span>{/if}
            <span title="misfire={s.misfire}">{MISFIRE[s.misfire] ?? s.misfire}</span>
            <span title="overlap={s.overlap}">{OVERLAP[s.overlap] ?? s.overlap}</span>
          </div>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="faint small">还没有定时，这个任务只能手动触发。</p>
  {/if}

  {#if error}<div class="banner">{error}</div>{/if}

</section>

<Modal open={adding} title="添加定时" onclose={() => (adding = false)}>
  <div class="presets">
    {#each PRESETS as p (p.expr)}
      <button class="btn-sm" class:on={cron === p.expr} onclick={() => (cron = p.expr)}>
        {p.label}
      </button>
    {/each}
  </div>
  <div class="form-grid">
    <Field label="cron 表达式" error={fieldErr.cron}>
      {#snippet control(p)}
        <input {...p} bind:value={cron} class="mono" placeholder="0 2 * * *" spellcheck="false" />
      {/snippet}
    </Field>
    <Field label="时区" error={fieldErr.timezone}>
      {#snippet control(p)}
        <input {...p} bind:value={timezone} placeholder="Asia/Shanghai" />
      {/snippet}
    </Field>
    <Field label="错过了怎么办" error={fieldErr.misfire}>
      {#snippet control(p)}
        <select {...p} bind:value={misfire}>
          <option value="skip">不补</option>
          <option value="fire_once">只补一次</option>
          <option value="fire_all">全部补上</option>
        </select>
      {/snippet}
    </Field>
    <Field label="上次没跑完" error={fieldErr.overlap}>
      {#snippet control(p)}
        <select {...p} bind:value={overlap}>
          <option value="skip">跳过这次</option>
          <option value="allow">允许并行</option>
          <option value="queue">排队</option>
        </select>
      {/snippet}
    </Field>
    <Field label="抖动（秒）" error={fieldErr.jitter_s}>
      {#snippet control(p)}
        <input {...p} bind:value={jitter} type="number" min="0" max="3600" />
      {/snippet}
    </Field>
  </div>
  <p class="faint small">
    支持 5 段（分 时 日 月 周）和 6 段（秒 分 时 日 月 周），以及 <code>L</code>、<code>#</code>。
    表达式在保存时就会编译一次，跑不通会当场拒绝。
  </p>
  {#snippet footer()}
    <button class="btn-ghost" onclick={() => (adding = false)} disabled={busy}>取消</button>
    <button class="btn-primary" onclick={create} disabled={busy || !cron}>添加</button>
  {/snippet}
</Modal>

<style>
  .items {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .items li {
    border: 1px solid var(--line);
    border-radius: var(--r2);
    padding: var(--s2) var(--s3);
    background: var(--surface-2);
  }
  .items li.off {
    opacity: 0.55;
  }
  .line {
    display: flex;
    gap: var(--s2);
    align-items: center;
    flex-wrap: wrap;
    font-size: var(--t-base);
  }
  .cron {
    font-size: var(--t-base);
    color: var(--fg);
  }
  .meta {
    margin-top: 2px;
    font-size: var(--t-xs);
    color: var(--fg-faint);
    gap: var(--s3);
  }
  .meta b {
    color: var(--fg-dim);
    font-weight: 500;
  }
  .new {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    margin-top: var(--s3);
    padding-top: var(--s3);
    border-top: 1px dashed var(--line-strong);
  }
  .presets {
    display: flex;
    gap: var(--s1);
    flex-wrap: wrap;
  }
  .presets button.on {
    border-color: var(--accent-dim);
    color: var(--accent-fg);
    background: var(--accent-soft);
  }
  .callout {
    margin-bottom: var(--s3);
  }
</style>
