<script lang="ts">
  /**
   * 一个任务的定时。
   *
   * 每一条先说人话（"工作日 09:00"），再说下次什么时候——光看 `0 9 * * 1-5`
   * 得在脑子里翻译一遍，而"下次 3 小时后"一眼就知道它是不是在按你以为的节奏跑。
   */
  import { session } from '$lib/auth/session.svelte';
  import { api, describeError } from '$api/client';
  import { listSchedules, type Schedule } from '$api/models';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { stamp, until } from '$lib/ui/format';
  import { describeCron } from './cron';
  import ScheduleEditor from './ScheduleEditor.svelte';

  let { taskId, taskEnabled = true }: { taskId: string; taskEnabled?: boolean } = $props();

  let items = $state<Schedule[]>([]);
  const canOperate = $derived(session.can('operator'));
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let busy = $state(false);

  /** 编辑器开着时改的是哪一条；`null` 且开着 = 新增。 */
  let editorOpen = $state(false);
  let editing = $state<Schedule | null>(null);

  const MISFIRE: Record<string, string> = {
    skip: '停服错过的不补',
    fire_once: '停服错过的补一次',
    fire_all: '停服错过的全补'
  };
  const OVERLAP: Record<string, string> = {
    skip: '上次没跑完就跳过',
    allow: '允许同时跑',
    queue: '上次没跑完就排队'
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

  function openEditor(s: Schedule | null) {
    editing = s;
    editorOpen = true;
  }

  async function toggle(s: Schedule) {
    busy = true;
    try {
      await api(`/api/v1/schedules/${s.id}/enabled`, { method: 'PUT', body: { enabled: !s.enabled } });
      toast(s.enabled ? '定时已停用' : '定时已启用');
      await load();
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = false;
    }
  }

  let pendingDelete = $state<Schedule | null>(null);
  async function remove(s: Schedule) {
    busy = true;
    try {
      await api(`/api/v1/schedules/${s.id}`, { method: 'DELETE' });
      toast('定时已删除');
      await load();
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = false;
    }
  }

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
  {#if pendingDelete}
    <p><b>{describeCron(pendingDelete.cron)}</b> · {pendingDelete.timezone}</p>
  {/if}
  <p>删掉之后这个任务就不会在这个时间自己跑了。已经产生的执行记录不受影响。</p>
</Confirm>

<ScheduleEditor
  open={editorOpen}
  {taskId}
  {editing}
  onclose={() => (editorOpen = false)}
  onsaved={() => {
    editorOpen = false;
    void load();
  }}
/>

<section class="card">
  <header class="card-head">
    <h2>定时</h2>
    <span class="spacer"></span>
    <button
      class="btn-sm"
      onclick={() => openEditor(null)}
      disabled={!canOperate}
      title={canOperate ? undefined : '需要 operator 权限'}
    >
      <Icon name="plus" size={12} />
      添加
    </button>
  </header>

  {#if !taskEnabled && items.some((s) => s.enabled)}
    <p class="callout warn small">任务已停用：定时到点也<strong>不会</strong>触发。启用任务后恢复。</p>
  {/if}

  {#if !loaded}
    <Loading rows={2} />
  {:else if items.length}
    <ul class="items">
      {#each items as s (s.id)}
        {@const text = describeCron(s.cron)}
        <li class:off={!s.enabled}>
          <div class="line">
            <span class="what">{text}</span>
            <span class="spacer"></span>
            <div class="row">
              <button
                class="btn-ghost btn-sm btn-icon"
                disabled={busy || !canOperate}
                title="修改"
                aria-label="修改定时"
                onclick={() => openEditor(s)}
              >
                <Icon name="pencil" />
              </button>
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
          </div>
          <div class="detail">
            {#if !s.enabled}
              <span>已停用</span>
            {:else if s.next_fire_at}
              <span title={s.next_three.join('\n')}>下次 <b>{until(s.next_fire_at)}</b></span>
            {:else}
              <span class="warn-text">算不出下一次</span>
            {/if}
            {#if s.last_fired_at}<span>上次 {stamp(s.last_fired_at)}</span>{/if}
            <span>{s.timezone}</span>
            {#if text !== s.cron}<code>{s.cron}</code>{/if}
          </div>
          <div class="detail policy">
            <span>{MISFIRE[s.misfire] ?? s.misfire}</span>
            <span>{OVERLAP[s.overlap] ?? s.overlap}</span>
            {#if s.jitter_s > 0}<span>随机延迟 ≤{s.jitter_s}s</span>{/if}
          </div>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="faint small">还没有定时，这个任务只能手动触发。</p>
  {/if}

  {#if error}<div class="banner">{error}</div>{/if}
</section>

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
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .items li.off {
    opacity: 0.6;
  }
  .line {
    display: flex;
    gap: var(--s2);
    align-items: center;
  }
  .what {
    font-weight: 600;
  }
  .detail {
    display: flex;
    flex-wrap: wrap;
    gap: 0 var(--s3);
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
  .detail b {
    color: var(--fg-dim);
    font-weight: 500;
  }
  .detail code {
    font-size: var(--t-xs);
  }
  .callout {
    margin-bottom: var(--s3);
  }
</style>
