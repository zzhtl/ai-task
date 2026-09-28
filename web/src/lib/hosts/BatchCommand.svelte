<script lang="ts">
  /**
   * 临时批量执行：在几台机器上各跑一次同一条命令，不用先建任务。
   *
   * 边写命令边看策略怎么判（停手 400ms 判一次，旧请求作废）：被拒的机器不能执行，
   * 需要确认的要显式勾上。执行结果落成一条执行记录，跳过去看每台机器的输出。
   */
  import { untrack } from 'svelte';
  import { goto } from '$app/navigation';
  import { ApiFailure, describeError, fieldErrors } from '$api/client';
  import { checkCommand, runCommand, type Host } from '$api/models';
  import { newIdempotencyKey } from '$api/runs';
  import type { CommandCheck } from '$api/types/CommandCheck';
  import type { HostSelector } from '$api/types/HostSelector';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';

  let {
    open,
    hosts,
    selected,
    onclose
  }: {
    open: boolean;
    hosts: Host[];
    /** 列表里勾选的主机。 */
    selected: string[];
    onclose: () => void;
  } = $props();

  const EFFECT_NAME = { allow: '放行', ask: '需要确认', deny: '拒绝' } as const;
  const EFFECT_TAG = { allow: 'ok', ask: 'warn', deny: 'danger' } as const;

  let mode = $state<'picked' | 'tag'>('picked');
  let tag = $state('');
  let command = $state('');
  let timeoutS = $state(300);
  let confirmed = $state(false);
  let check = $state<CommandCheck | null>(null);
  let checkError = $state<string | null>(null);
  let errors = $state<Record<string, string>>({});
  let running = $state(false);

  const tagCounts = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const h of hosts) for (const t of h.tags) counts.set(t, (counts.get(t) ?? 0) + 1);
    return [...counts.entries()].sort((a, b) => (a[0] < b[0] ? -1 : 1));
  });

  // 每次打开从勾选的那几台起步；一台都没勾就默认按 tag。
  // 只跟着 `open` 跑：读到别的状态的话，之后换个 tag 都会把模式拨回去
  $effect(() => {
    if (!open) return;
    untrack(() => {
      mode = selected.length ? 'picked' : 'tag';
      if (!tag) tag = tagCounts[0]?.[0] ?? '';
      confirmed = false;
      errors = {};
    });
  });

  /** 执行时被 409 挡回来（判完之后规则被改了），让判决重新跑一次。 */
  let recheck = $state(0);

  const targets = $derived<HostSelector | null>(
    mode === 'picked'
      ? selected.length
        ? { on: 'hosts', host_ids: [...selected] }
        : null
      : tag
        ? { on: 'tag', tag }
        : null
  );

  $effect(() => {
    void recheck;
    const text = command.trim();
    const t = targets;
    if (!open || !text || !t) {
      check = null;
      checkError = null;
      return;
    }
    const controller = new AbortController();
    const timer = setTimeout(async () => {
      try {
        check = await checkCommand({ command: text, targets: t }, controller.signal);
        checkError = null;
      } catch (e) {
        if (controller.signal.aborted) return;
        check = null;
        checkError = describeError(e);
      }
    }, 400);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  });

  // 判决变了（改了命令、换了机器）就要重新确认：确认的是那一份判决，不是这个弹层
  $effect(() => {
    void check;
    confirmed = false;
  });

  const blocked = $derived(!check || check.denied > 0 || (check.needs_confirmation > 0 && !confirmed));

  async function execute() {
    if (!targets || blocked) return;
    running = true;
    errors = {};
    try {
      const run = await runCommand(
        { command: command.trim(), targets, timeout_s: timeoutS, confirm: confirmed },
        newIdempotencyKey()
      );
      toast(`已在 ${check?.hosts.length ?? 0} 台机器上开始执行`);
      onclose();
      await goto(`/runs/${run.id}`);
    } catch (e) {
      // 409：判完之后规则被人改了。把新的判决拉回来，让人重新看一眼
      if (e instanceof ApiFailure && e.status === 409) {
        toastError(describeError(e));
        recheck += 1;
      } else {
        errors = fieldErrors(e);
        if (!Object.keys(errors).length) checkError = describeError(e);
      }
    } finally {
      running = false;
    }
  }
</script>

<Modal {open} title="在多台机器上执行命令" size="lg" {onclose}>
  <div class="form-grid">
    <div class="wide targets">
      <span class="label">在哪些机器上</span>
      <div class="seg" role="group" aria-label="选机器的方式">
        <button class:on={mode === 'picked'} aria-pressed={mode === 'picked'} onclick={() => (mode = 'picked')}>
          已勾选的 {selected.length} 台
        </button>
        <button class:on={mode === 'tag'} aria-pressed={mode === 'tag'} onclick={() => (mode = 'tag')}>按 tag</button>
      </div>
      {#if mode === 'tag'}
        <select bind:value={tag} aria-label="tag">
          {#each tagCounts as [t, n] (t)}<option value={t}>{t}（{n} 台）</option>{/each}
        </select>
      {:else if selected.length === 0}
        <span class="faint small">先在列表里勾几台机器，或者按 tag 选</span>
      {/if}
      {#if errors.targets}<span class="field-error">{errors.targets}</span>{/if}
    </div>
    <Field label="命令" hint="交给目标机上的 sh -c；每台各跑一次、互不影响" error={errors.command} wide>
      {#snippet control(p)}
        <textarea {...p} bind:value={command} rows="3" class="mono" spellcheck="false" placeholder="df -h /"></textarea>
      {/snippet}
    </Field>
    <Field label="每台的超时（秒）" error={errors.timeout_s}>
      {#snippet control(p)}
        <input {...p} type="number" min="1" max="3600" bind:value={timeoutS} />
      {/snippet}
    </Field>
  </div>

  <div class="verdicts" aria-live="polite">
    {#if checkError}
      <p class="field-error">{checkError}</p>
    {:else if check}
      <p class="summary">
        策略判决：{check.hosts.length} 台中
        {#if check.denied}<span class="bad-text">拒绝 {check.denied}</span>{/if}
        {#if check.needs_confirmation}<span class="warn-text">需要确认 {check.needs_confirmation}</span>{/if}
        <span>放行 {check.hosts.length - check.denied - check.needs_confirmation}</span>
      </p>
      <ul>
        {#each check.hosts as h (h.host_id)}
          <li>
            <span class="name">{h.host_name}</span>
            <span class="tag {EFFECT_TAG[h.effect]}">{EFFECT_NAME[h.effect]}</span>
            <span class="faint small reason">{h.rule_name ? `「${h.rule_name}」：` : ''}{h.reason}</span>
          </li>
        {/each}
      </ul>
      {#if check.denied}
        <p class="bad-text small">有机器被策略拒绝，这条命令不能执行。去掉那几台，或者改命令。</p>
      {:else if check.needs_confirmation}
        <label class="confirm">
          <input type="checkbox" bind:checked={confirmed} />
          我看过了，确认要在需要确认的 {check.needs_confirmation} 台机器上执行
        </label>
      {/if}
    {:else if command.trim() && targets}
      <p class="faint small">判着…</p>
    {/if}
  </div>

  {#snippet footer()}
    <button class="btn-ghost" onclick={onclose} disabled={running}>取消</button>
    <button class="btn-primary" onclick={execute} disabled={running || blocked || !targets}>
      {running ? '下发中…' : `在 ${check?.hosts.length ?? 0} 台机器上执行`}
    </button>
  {/snippet}
</Modal>

<style>
  .targets {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--s2);
  }
  .label {
    flex-basis: 100%;
    font-size: var(--t-xs);
    color: var(--fg-dim);
  }
  .targets select {
    width: auto;
    min-width: 12rem;
  }
  .verdicts {
    margin-top: var(--s3);
    padding-top: var(--s3);
    border-top: 1px solid var(--line);
    min-height: 3rem;
  }
  .summary {
    display: flex;
    gap: var(--s3);
    margin: 0 0 var(--s2);
    font-size: var(--t-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 14rem;
    overflow: auto;
  }
  li {
    display: flex;
    align-items: baseline;
    gap: var(--s2);
    padding: 3px 0;
  }
  .name {
    font-weight: 500;
    white-space: nowrap;
  }
  .reason {
    min-width: 0;
  }
  .confirm {
    display: flex;
    align-items: center;
    gap: var(--s2);
    margin-top: var(--s2);
    font-size: var(--t-sm);
  }
  .confirm input {
    width: auto;
    height: auto;
  }
  .bad-text {
    color: var(--bad-fg);
  }
</style>
