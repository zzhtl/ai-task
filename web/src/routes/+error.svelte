<script lang="ts">
  /**
   * 路由级错误边界。
   *
   * 在这之前整个应用没有任何边界：一个组件里抛出的异常会把界面变成白屏，
   * 而这是个用来盯执行状态的控制台——白屏和"什么都没发生"看起来一样。
   */
  import { page } from '$app/state';
  import PageHeader from '$lib/ui/PageHeader.svelte';
</script>

<PageHeader title="这一页出错了" />

<div class="banner">
  <div>
    <strong>{page.status}</strong>
    {page.error?.message ?? '未知错误'}
  </div>
</div>

<div class="card">
  <p class="muted">
    这不是服务端返回的业务错误，而是界面自己崩了。刷新通常能恢复；如果反复出现，
    浏览器控制台里的堆栈是唯一有用的线索。
  </p>
  <div class="form-actions">
    <button class="btn-primary" onclick={() => location.reload()}>刷新</button>
    <a class="btn" href="/">回到概览</a>
  </div>
</div>
