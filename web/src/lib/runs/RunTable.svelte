<script lang="ts">
  /**
   * 执行记录表。执行记录页和任务详情共用，行为必须一致：失败原因直接写在行里、
   * 进行中的耗时实时走、停在审批门上的标成"等待审批"、结束了的能原样重跑。
   *
   * 翻页和轮询由调用方管（两处的查询条件不一样），这里只管画和行内动作。
   */
  import { goto } from '$app/navigation';
  import { deleteRun } from '$api/runs';
  import { rerun as rerunRun } from './rerun';
  import { describeError } from '$api/client';
  import { resource } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import type { RunListItem } from '$api/types/RunListItem';
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import Confirm from '$lib/ui/Confirm.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, duration, money, stamp, triggerLabel } from '$lib/ui/format';

  let {
    runs,
    showTask = true,
    onremoved
  }: {
    runs: RunListItem[];
    /** 任务详情里每一行都是同一个任务，任务列换成触发方式。 */
    showTask?: boolean;
    /** 删掉一行之后调用方把它从自己的列表里拿掉。 */
    onremoved: (id: string) => void;
  } = $props();

  const canOperate = $derived(session.can('operator'));
  const isLive = (r: RunListItem) => r.status === 'running' || r.status === 'queued';

  // 停在审批门上的 run 状态还是 running。单独标出来：那是在等人，不是在跑。
  // 和侧栏徽标共享同一个 key，不多发请求
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });
  const awaitingRuns = $derived(new Set((approvals.data ?? []).map((a) => a.run_id)));

  // 有进行中的行时，耗时每秒走一次
  let now = $state(Date.now());
  const anyLive = $derived(runs.some(isLive));
  $effect(() => {
    if (!anyLive) return;
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const elapsed = (r: RunListItem) => {
    void now;
    return duration(r.started_at, r.finished_at);
  };

  /** 整行可点，但行里的按钮、链接、菜单要各干各的。 */
  function openRow(event: MouseEvent, id: string) {
    if ((event.target as HTMLElement).closest('a, button, [role="menu"]')) return;
    void goto(`/runs/${id}`);
  }

  /**
   * 删执行记录。**连事件流和资源采样一起删，不可恢复。**
   * 只能删终态的：还在跑的 run，执行器仍在往它的事件流里写。
   */
  let pendingDelete = $state<RunListItem | null>(null);
  let busy = $state<string | null>(null);

  async function remove(run: RunListItem) {
    busy = run.id;
    try {
      await deleteRun(run.id);
      onremoved(run.id);
      toast('已删除这条执行记录');
    } catch (e) {
      toastError(describeError(e));
    } finally {
      busy = null;
    }
  }

  /** 重跑：原样带上那一次的输入。 */
  async function rerun(run: RunListItem) {
    busy = run.id;
    try {
      const id = await rerunRun(run);
      toast(`已重新触发「${run.task_name}」`);
      await goto(`/runs/${id}`);
    } catch (e) {
      toastError(describeError(e));
      busy = null;
    }
  }
</script>

<Confirm
  open={pendingDelete !== null}
  onclose={() => (pendingDelete = null)}
  title="删除这次执行记录？"
  danger
  confirmText="删除"
  onconfirm={() => {
    const run = pendingDelete;
    pendingDelete = null;
    if (run) void remove(run);
  }}
>
  {#if pendingDelete}
    <p><b>{pendingDelete.task_name}</b> · {ago(pendingDelete.created_at)} · {money(pendingDelete.cost_usd)}</p>
    <p>连同它的完整事件流和资源采样一起删掉。那里面有成本记录和策略判决，是审计材料。</p>
    <p class="warn-text">删掉之后拿不回来。</p>
  {/if}
</Confirm>

<table>
  <thead>
    <tr>
      <th>状态</th>
      <th>{showTask ? '任务' : '触发'}</th>
      {#if showTask}<th>触发</th>{/if}
      <th>开始</th>
      <th class="num">耗时</th>
      <th class="num">花费</th>
      <th class="act"></th>
    </tr>
  </thead>
  <tbody>
    {#each runs as r (r.id)}
      <tr class="clickable" onclick={(e) => openRow(e, r.id)}>
        <td class="nowrap">
          <StatusBadge status={r.status === 'running' && awaitingRuns.has(r.id) ? 'awaiting_approval' : r.status} />
        </td>
        <td class="main-cell">
          <a href="/runs/{r.id}">{showTask ? r.task_name : triggerLabel(r.trigger)}</a>
          {#if r.dry_run}<span class="tag accent">影子</span>{/if}
          {#if r.error}
            <!-- 为了看一句「为什么失败」再点一次，是排查时最没必要的一次点击 -->
            <div class="err ellipsis" title={r.error}>{r.error}</div>
          {/if}
        </td>
        {#if showTask}<td class="faint nowrap">{triggerLabel(r.trigger)}</td>{/if}
        <td class="faint nowrap" title={stamp(r.started_at ?? r.created_at)}>{ago(r.started_at ?? r.created_at)}</td>
        <td class="num" class:faint={!isLive(r)} class:live={isLive(r)}>{elapsed(r)}</td>
        <td class="num faint">{money(r.cost_usd)}</td>
        <td class="act">
          <div class="row">
            {#if canOperate && !isLive(r)}
              <button
                class="btn-ghost btn-sm btn-icon"
                title="用同样的输入再跑一次"
                aria-label="重跑"
                disabled={busy === r.id}
                onclick={() => rerun(r)}
              >
                <Icon name="refresh" />
              </button>
            {/if}
            <button
              class="btn-ghost btn-sm btn-icon danger"
              title={!canOperate ? '需要 operator 权限' : isLive(r) ? '还在跑，先取消' : '删除这条执行记录'}
              aria-label="删除执行记录"
              disabled={isLive(r) || busy === r.id || !canOperate}
              onclick={() => (pendingDelete = r)}
            >
              <Icon name="trash" />
            </button>
          </div>
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  .main-cell {
    max-width: 44ch;
  }
  .main-cell a {
    font-weight: 500;
  }
  .main-cell .tag {
    margin-left: var(--s1);
  }
  .live {
    color: var(--info-fg);
  }
</style>
