<script lang="ts">
  /** 数据还没回来时的占位。先闪一下"没有数据"再变出内容，比多看一眼骨架更糟。 */
  let { rows = 3 }: { rows?: number } = $props();
  const WIDTHS = ['62%', '48%', '71%', '55%', '40%', '66%'];
</script>

<div class="skeleton" aria-busy="true" aria-label="加载中">
  {#each Array.from({ length: rows }) as _, i (i)}
    <span style="width: {WIDTHS[i % WIDTHS.length]}"></span>
  {/each}
</div>
