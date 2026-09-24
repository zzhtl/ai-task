<script lang="ts">
  /**
   * 步骤左侧的序号 + 连线。
   *
   * 编排编辑器、只读步骤列表、执行过程视图三处长得**必须**一模一样——
   * 同一个步骤在三个地方对不上，人就得停下来确认自己看的是不是同一件事。
   * 在这之前这 24 行样式在三个文件里逐字抄了三遍，改一处就会错开。
   */
  let {
    index,
    /** 最后一个不画向下的连线。 */
    last = false,
    /** 步骤类型决定序号的颜色；执行过程视图不分类型，不传即可。 */
    kind
  }: { index: number; last?: boolean; kind?: 'ai' | 'shell' | 'approval' } = $props();
</script>

<div class="rail">
  <span class="num {kind ?? ''}">{index}</span>
  {#if !last}<span class="wire"></span>{/if}
</div>

<style>
  .rail {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s1);
  }
  .num {
    width: 1.6rem;
    height: 1.6rem;
    border-radius: 50%;
    border: 1px solid var(--line-strong);
    background: var(--surface-2);
    display: grid;
    place-items: center;
    font-size: var(--t-sm);
    color: var(--fg-dim);
    flex: 0 0 auto;
  }
  .wire {
    flex: 1;
    width: 1px;
    background: var(--line);
    min-height: var(--s4);
  }
  .num.ai {
    border-color: var(--st-ai);
    color: var(--st-ai);
  }
  .num.approval {
    border-color: var(--warn-border);
    color: var(--warn-fg);
  }
</style>
