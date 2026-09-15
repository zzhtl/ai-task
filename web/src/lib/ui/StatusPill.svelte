<script lang="ts">
  /**
   * 状态显示。**全站唯一的状态渲染方式**——列表、过程、时间轴必须完全一致。
   * 同一个状态在两个地方长得不一样，人就会停下来确认，那正是要消除的东西。
   */
  let { status, label }: { status: string; label?: string } = $props();

  const TEXT: Record<string, string> = {
    queued: '排队中',
    running: '执行中',
    succeeded: '成功',
    failed: '失败',
    cancelled: '已取消',
    timed_out: '超时',
    budget_exceeded: '预算耗尽',
    resource_exceeded: '资源击穿',
    awaiting_approval: '等待审批',
    pending: '等待',
    ready: '就绪',
    skipped: '跳过'
  };
</script>

<span class="pill {status}">
  <span class="dot {status}"></span>
  {label ?? TEXT[status] ?? status}
</span>

<style>
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: var(--t-sm);
    color: var(--fg-dim);
    white-space: nowrap;
  }
  .pill.running {
    color: var(--st-running);
  }
  .pill.succeeded {
    color: var(--st-succeeded);
  }
  .pill.failed {
    color: var(--st-failed);
  }
  .pill.timed_out,
  .pill.budget_exceeded,
  .pill.awaiting_approval {
    color: var(--st-timeout);
  }
  .pill.resource_exceeded {
    color: var(--st-resource);
  }
</style>
