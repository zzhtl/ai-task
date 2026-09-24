<script lang="ts">
  /**
   * 新增 / 修改一条定时。
   *
   * 默认用构建器（每天 / 每周 / 每月 / 间隔）：手写 cron 是配定时最容易错的地方，
   * `0 0 * * *` 和 `0 0 * * 0` 只差一个字符。构建器表示不了的才落到自定义表达式。
   *
   * 边改边看接下来 5 次在什么时候触发。这是"写对了没有"最直接的证据，
   * 等存完再看就晚了——存错的定时不报错，只是不在你以为的时候跑。
   */
  import { untrack } from 'svelte';
  import { describeError, fieldErrors } from '$api/client';
  import { previewSchedule, saveSchedule, type Schedule } from '$api/models';
  import type { ScheduleFire } from '$api/types/ScheduleFire';
  import Modal from '$lib/ui/Modal.svelte';
  import { DISPLAY_TIMEZONE, until } from '$lib/ui/format';
  import { toast } from '$lib/ui/toast.svelte';
  import { describeBuilder, fromCron, INTERVALS, toCron, type CronBuilder, type IntervalUnit } from './cron';

  let {
    open,
    taskId,
    editing = null,
    onclose,
    onsaved
  }: {
    open: boolean;
    taskId: string;
    /** 修改哪一条。`null` = 新增。 */
    editing?: Schedule | null;
    onclose: () => void;
    onsaved: () => void;
  } = $props();

  let builder = $state<CronBuilder>({ mode: 'daily', time: '02:00' });
  let timezone = $state(DISPLAY_TIMEZONE);
  let misfire = $state('fire_once');
  let overlap = $state('skip');
  let jitter = $state(0);
  let busy = $state(false);
  let saveErrors = $state<Record<string, string>>({});
  let saveError = $state<string | null>(null);

  // 每次打开都从要改的那一条（或默认值）重新开始，上次没保存的草稿不带过来
  $effect(() => {
    if (!open) return;
    const s = editing;
    untrack(() => {
      builder = s ? fromCron(s.cron) : { mode: 'daily', time: '02:00' };
      timezone = s?.timezone ?? DISPLAY_TIMEZONE;
      misfire = s?.misfire ?? 'fire_once';
      overlap = s?.overlap ?? 'skip';
      jitter = s?.jitter_s ?? 0;
      saveErrors = {};
      saveError = null;
    });
  });

  const cron = $derived(toCron(builder));
  /** 认不出的表达式翻译出来就是它自己。 */
  const described = $derived(cron ? describeBuilder(fromCron(cron)) : '');

  const PRESETS = [
    { label: '每天 02:00', expr: '0 2 * * *' },
    { label: '工作日 09:00', expr: '0 9 * * 1-5' },
    { label: '每小时', expr: '0 * * * *' },
    { label: '每 5 分钟', expr: '*/5 * * * *' },
    { label: '每月 1 号 03:00', expr: '0 3 1 * *' }
  ];

  const MODES: Array<{ id: CronBuilder['mode']; label: string }> = [
    { id: 'daily', label: '每天' },
    { id: 'weekly', label: '每周' },
    { id: 'monthly', label: '每月' },
    { id: 'interval', label: '间隔' },
    { id: 'custom', label: '自定义' }
  ];

  /** 换模式时尽量留住已经填的时间；换到自定义时把当前的表达式带过去接着改。 */
  function setMode(mode: CronBuilder['mode']) {
    if (mode === builder.mode) return;
    const time = 'time' in builder && builder.time ? builder.time : '02:00';
    if (mode === 'daily') builder = { mode, time };
    else if (mode === 'weekly') builder = { mode, days: [1], time };
    else if (mode === 'monthly') builder = { mode, day: 1, time };
    else if (mode === 'interval') builder = { mode, every: 1, unit: 'hour' };
    else builder = { mode, expr: cron || '0 2 * * *' };
  }

  // 周一在前、周日在后，是中文里说"周几"的顺序；值还是 cron 的编号
  const WEEK = [
    { d: 1, label: '一' },
    { d: 2, label: '二' },
    { d: 3, label: '三' },
    { d: 4, label: '四' },
    { d: 5, label: '五' },
    { d: 6, label: '六' },
    { d: 0, label: '日' }
  ];
  function toggleDay(d: number) {
    if (builder.mode !== 'weekly') return;
    builder.days = builder.days.includes(d) ? builder.days.filter((x) => x !== d) : [...builder.days, d];
  }

  const UNITS: Array<{ id: IntervalUnit; label: string }> = [
    { id: 'second', label: '秒' },
    { id: 'minute', label: '分钟' },
    { id: 'hour', label: '小时' }
  ];
  /** 换单位时挑一个新单位下合法、又最接近原值的档位。 */
  function setUnit(unit: IntervalUnit) {
    if (builder.mode !== 'interval') return;
    const every = builder.every;
    const options = INTERVALS[unit];
    builder = {
      mode: 'interval',
      unit,
      every: options.reduce((best, n) => (Math.abs(n - every) < Math.abs(best - every) ? n : best), options[0])
    };
  }

  const TIMEZONES = (() => {
    try {
      return Intl.supportedValuesOf('timeZone');
    } catch {
      return ['Asia/Shanghai', 'UTC'];
    }
  })();

  // ---------------------------------------------------------------- 预览

  type Preview =
    | { kind: 'empty' }
    | { kind: 'loading' }
    | { kind: 'ok'; fires: ScheduleFire[] }
    | { kind: 'error'; fields: Record<string, string>; message: string };
  let preview = $state<Preview>({ kind: 'empty' });

  /** 敲一个字符发一次太多，停下来 300ms 再算；新的一次发出去，旧的就作废。 */
  $effect(() => {
    const expr = cron;
    const tz = timezone.trim();
    if (!open) return;
    if (!expr || !tz) {
      preview = { kind: 'empty' };
      return;
    }
    preview = { kind: 'loading' };
    const ctrl = new AbortController();
    const timer = setTimeout(() => {
      previewSchedule(expr, tz, 5, ctrl.signal)
        .then((p) => (preview = { kind: 'ok', fires: p.fires }))
        .catch((e) => {
          if (ctrl.signal.aborted) return;
          preview = { kind: 'error', fields: fieldErrors(e), message: describeError(e) };
        });
    }, 300);
    return () => {
      clearTimeout(timer);
      ctrl.abort();
    };
  });

  const errorOf = (field: string) =>
    saveErrors[field] ?? (preview.kind === 'error' ? preview.fields[field] : undefined);

  /** `2026-09-25 02:00:00 CST` → `09-25 周四 02:00`。秒不是 0 才显示秒（每 30 秒那种）。 */
  function fireLabel(local: string): string {
    const m = /^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2}):(\d{2})/.exec(local);
    if (!m) return local;
    const weekday = '日一二三四五六'[new Date(Date.UTC(+m[1], +m[2] - 1, +m[3])).getUTCDay()];
    return `${m[2]}-${m[3]} 周${weekday} ${m[4]}:${m[5]}${m[6] === '00' ? '' : `:${m[6]}`}`;
  }

  const canSave = $derived(!busy && cron !== '' && preview.kind === 'ok' && preview.fires.length > 0);

  async function save() {
    busy = true;
    saveErrors = {};
    saveError = null;
    try {
      await saveSchedule(
        {
          task_id: taskId,
          cron,
          timezone: timezone.trim(),
          misfire,
          overlap,
          jitter_s: Number(jitter) || 0,
          enabled: editing?.enabled ?? true
        },
        editing?.id
      );
      toast(editing ? '定时已修改' : '定时已添加');
      onsaved();
    } catch (e) {
      saveErrors = fieldErrors(e);
      saveError = Object.keys(saveErrors).length ? null : describeError(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal {open} title={editing ? '修改定时' : '添加定时'} size="lg" {onclose}>
  <div class="editor">
    <div class="presets" role="group" aria-label="常用">
      {#each PRESETS as p (p.expr)}
        <button class="btn-sm" class:on={cron === p.expr} onclick={() => (builder = fromCron(p.expr))}>
          {p.label}
        </button>
      {/each}
    </div>

    <div class="seg" role="group" aria-label="怎么重复">
      {#each MODES as m (m.id)}
        <button class:on={builder.mode === m.id} onclick={() => setMode(m.id)}>{m.label}</button>
      {/each}
    </div>

    <div class="build">
      {#if builder.mode === 'daily'}
        <label class="field inline">
          每天几点
          <input type="time" bind:value={builder.time} required />
        </label>
      {:else if builder.mode === 'weekly'}
        <div class="field">
          <span>每周哪几天</span>
          <div class="days">
            {#each WEEK as w (w.d)}
              <button
                class="btn-sm day"
                class:on={builder.days.includes(w.d)}
                aria-pressed={builder.days.includes(w.d)}
                onclick={() => toggleDay(w.d)}>周{w.label}</button
              >
            {/each}
          </div>
          {#if builder.days.length === 0}<span class="hint warn-text">至少选一天</span>{/if}
        </div>
        <label class="field inline">
          几点
          <input type="time" bind:value={builder.time} required />
        </label>
      {:else if builder.mode === 'monthly'}
        <label class="field inline">
          每月哪天
          <select
            value={String(builder.day)}
            onchange={(e) => {
              if (builder.mode !== 'monthly') return;
              const v = e.currentTarget.value;
              builder.day = v === 'last' ? 'last' : Number(v);
            }}
          >
            {#each { length: 31 }, i}<option value={String(i + 1)}>{i + 1} 号</option>{/each}
            <option value="last">最后一天</option>
          </select>
        </label>
        <label class="field inline">
          几点
          <input type="time" bind:value={builder.time} required />
        </label>
        {#if typeof builder.day === 'number' && builder.day > 28}
          <p class="hint">没有 {builder.day} 号的月份这一次会跳过。想要每个月都跑，选"最后一天"。</p>
        {/if}
      {:else if builder.mode === 'interval'}
        <label class="field inline">
          每隔
          <select
            value={String(builder.every)}
            onchange={(e) => {
              if (builder.mode === 'interval') builder.every = Number(e.currentTarget.value);
            }}
          >
            {#each INTERVALS[builder.unit] as n (n)}<option value={String(n)}>{n}</option>{/each}
          </select>
          <select value={builder.unit} onchange={(e) => setUnit(e.currentTarget.value as IntervalUnit)}>
            {#each UNITS as u (u.id)}<option value={u.id}>{u.label}</option>{/each}
          </select>
        </label>
        <p class="hint">只给能整除的间隔：cron 里的"每 7 分钟"到整点会重新数，并不均匀。</p>
      {:else}
        <label class="field">
          cron 表达式
          <input
            class="mono"
            bind:value={builder.expr}
            placeholder="0 2 * * *"
            spellcheck="false"
            aria-invalid={errorOf('cron') ? 'true' : undefined}
          />
          <span class="hint">5 段（分 时 日 月 周）或 6 段（秒 分 时 日 月 周），支持 <code>L</code>、<code>#</code>。</span>
        </label>
      {/if}
      {#if errorOf('cron')}<p class="field-error">{errorOf('cron')}</p>{/if}
    </div>

    <label class="field">
      时区
      <input
        bind:value={timezone}
        list="tz-options"
        placeholder="Asia/Shanghai"
        spellcheck="false"
        aria-invalid={errorOf('timezone') ? 'true' : undefined}
      />
      <datalist id="tz-options">
        {#each TIMEZONES as tz (tz)}<option value={tz}></option>{/each}
      </datalist>
      {#if errorOf('timezone')}<span class="field-error">{errorOf('timezone')}</span>{/if}
    </label>

    <section class="preview" aria-live="polite">
      <header>
        <b>{!cron ? '还没填完' : described === cron ? '自定义表达式' : described}</b>
        {#if cron}<code class="faint">{cron}</code>{/if}
      </header>
      {#if preview.kind === 'ok'}
        <ul>
          {#each preview.fires as f (f.at)}
            <li><span class="when">{fireLabel(f.local)}</span><span class="faint">{until(f.at)}</span></li>
          {/each}
        </ul>
      {:else if preview.kind === 'loading'}
        <p class="faint small">算接下来几次…</p>
      {:else if preview.kind === 'error' && Object.keys(preview.fields).length === 0}
        <p class="field-error">{preview.message}</p>
      {/if}
    </section>

    <details class="advanced">
      <summary>高级：错过了怎么办、上次没跑完、抖动</summary>
      <div class="form-grid">
        <label class="field">
          服务停着时错过的
          <select bind:value={misfire}>
            <option value="skip">不补</option>
            <option value="fire_once">只补一次</option>
            <option value="fire_all">全部补上</option>
          </select>
        </label>
        <label class="field">
          到点时上次还没跑完
          <select bind:value={overlap}>
            <option value="skip">跳过这次</option>
            <option value="queue">排队等上次跑完</option>
            <option value="allow">允许同时跑</option>
          </select>
        </label>
        <label class="field">
          随机延迟（秒）
          <input type="number" bind:value={jitter} min="0" max="3600" />
          <span class="hint">避免很多任务同一秒一起启动；记录里的触发时间不受影响。</span>
          {#if errorOf('jitter_s')}<span class="field-error">{errorOf('jitter_s')}</span>{/if}
        </label>
      </div>
    </details>

    {#if saveError}<div class="banner">{saveError}</div>{/if}
  </div>

  {#snippet footer()}
    <button class="btn-ghost" onclick={onclose} disabled={busy}>取消</button>
    <button class="btn-primary" onclick={save} disabled={!canSave}>
      {busy ? '保存中…' : editing ? '保存修改' : '添加'}
    </button>
  {/snippet}
</Modal>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: var(--s4);
  }
  .presets {
    display: flex;
    gap: var(--s1);
    flex-wrap: wrap;
  }
  .presets button.on,
  .day.on {
    border-color: var(--accent-border);
    color: var(--accent-fg);
    background: var(--accent-soft);
  }
  .seg {
    align-self: flex-start;
  }
  .build {
    display: flex;
    flex-wrap: wrap;
    gap: var(--s3) var(--s4);
    align-items: flex-end;
  }
  .build > .field:not(.inline),
  .build > .hint,
  .build > .field-error {
    flex-basis: 100%;
  }
  .field.inline {
    flex-direction: row;
    align-items: center;
    gap: var(--s2);
  }
  /* 全局只给 label.field 定了样式；一组按钮套不进 label */
  div.field {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    font-size: var(--t-sm);
    font-weight: 500;
    color: var(--fg-dim);
  }
  .days {
    display: flex;
    gap: var(--s1);
    flex-wrap: wrap;
  }
  .hint {
    margin: 0;
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .preview {
    border: 1px solid var(--line);
    border-radius: var(--r2);
    background: var(--surface-2);
    padding: var(--s3);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .preview header {
    display: flex;
    gap: var(--s2);
    align-items: baseline;
    flex-wrap: wrap;
  }
  .preview ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: var(--t-sm);
  }
  .preview li {
    display: flex;
    justify-content: space-between;
    gap: var(--s3);
  }
  .when {
    font-variant-numeric: tabular-nums;
  }
  .advanced summary {
    cursor: pointer;
    font-size: var(--t-sm);
    color: var(--fg-dim);
  }
  .advanced .form-grid {
    margin-top: var(--s3);
  }
</style>
