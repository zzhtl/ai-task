<script lang="ts">
  /**
   * 应用外壳：常驻侧栏 + 命令面板 + 主题切换。
   *
   * 每个页面各自放一个「← 返回」是 1998 年的做法：它假设用户是顺着一条路进来的，
   * 而实际上人是从告警、从链接、从上一次的标签页里跳进来的。常驻导航让「我在哪、
   * 能去哪」永远可见。
   */
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { session, logout } from '$lib/auth/session.svelte';
  import { resource } from '$api/resource.svelte';
  import { listApprovals, type Approval } from '$api/models';
  import { theme } from './theme.svelte';
  import CommandPalette from './CommandPalette.svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';

  let { children } = $props();

  let paletteOpen = $state(false);
  /** 窄屏上侧栏收进抽屉。宽屏用不到这个状态。 */
  let drawerOpen = $state(false);

  // 点了导航就把抽屉收起来——不收的话跳过去之后抽屉还盖在内容上
  $effect(() => {
    void page.url.pathname;
    drawerOpen = false;
  });
  // 和首页、/approvals、run 详情页共享同一个 key：四处订阅只产生一条请求。
  // 之前是四个各自的 setInterval，闲置首页上 /api/v1/approvals 一分钟被打约 27 次。
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });
  const pendingApprovals = $derived(approvals.data?.length ?? 0);

  interface NavItem {
    href: string;
    label: string;
    icon: IconName;
    admin?: boolean;
    badge?: () => number;
  }
  interface NavGroup {
    label: string | null;
    admin?: boolean;
    items: NavItem[];
  }

  const GROUPS: NavGroup[] = [
    {
      label: null,
      items: [
        { href: '/', label: '概览', icon: 'activity' },
        { href: '/tasks', label: '任务', icon: 'list' },
        { href: '/runs', label: '执行', icon: 'play' },
        {
          href: '/approvals',
          label: '审批',
          icon: 'shield-check',
          badge: () => pendingApprovals
        }
      ]
    },
    {
      label: '管理',
      admin: true,
      items: [
        { href: '/hosts', label: '主机', icon: 'server' },
        { href: '/rules', label: '规则', icon: 'shield' },
        { href: '/users', label: '用户', icon: 'users' },
        { href: '/audit', label: '审计', icon: 'file' }
      ]
    }
  ];

  const groups = $derived(GROUPS.filter((g) => !g.admin || session.can('admin')));

  function active(href: string): boolean {
    return href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
  }

  function onKey(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      paletteOpen = !paletteOpen;
    }
  }

  const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);
</script>

<svelte:window onkeydown={onKey} />

<!-- 窄屏的顶栏。宽屏上 display:none——侧栏一直在，不需要它 -->
<header class="topbar">
  <button
    class="btn-ghost btn-icon"
    aria-label="打开导航"
    aria-expanded={drawerOpen}
    onclick={() => (drawerOpen = true)}
  >
    <Icon name="list" />
  </button>
  <a class="brand" href="/">
    <span class="mark"></span>
    <span class="name">ai-task</span>
  </a>
  <span class="spacer"></span>
  <button class="btn-ghost btn-icon" aria-label="搜索" onclick={() => (paletteOpen = true)}>
    <Icon name="search" />
  </button>
</header>

<div class="shell" class:drawer-open={drawerOpen}>
  <!-- 抽屉打开时点背景收起。宽屏上这块 display:none -->
  <div
    class="scrim"
    role="presentation"
    onclick={() => (drawerOpen = false)}
    aria-hidden="true"
  ></div>
  <nav aria-label="主导航">
    <a class="brand" href="/">
      <span class="mark"></span>
      <span class="name">ai-task</span>
    </a>

    <button class="search" onclick={() => (paletteOpen = true)} title="搜索任务、执行记录">
      <Icon name="search" />
      <span class="label">搜索…</span>
      <span class="kbd">{isMac ? '⌘' : 'Ctrl'} K</span>
    </button>

    {#each groups as group (group.label ?? 'main')}
      <div class="group">
        {#if group.label}<span class="group-label">{group.label}</span>{/if}
        <ul>
          {#each group.items as item (item.href)}
            <li>
              <a href={item.href} class:active={active(item.href)} title={item.label}>
                <Icon name={item.icon} />
                <span class="label">{item.label}</span>
                {#if item.badge?.()}
                  <span class="badge">{item.badge()}</span>
                {/if}
              </a>
            </li>
          {/each}
        </ul>
      </div>
    {/each}

    <div class="spacer"></div>

    <div class="foot">
      <button
        class="btn-ghost btn-icon"
        title={theme.current === 'dark' ? '切到浅色' : '切到深色'}
        aria-label="切换主题"
        onclick={() => theme.toggle()}
      >
        {#if theme.current === 'dark'}
          <Icon name="sun" />
        {:else}
          <Icon name="moon" />
        {/if}
      </button>

      {#if session.identity}
        <!-- 名字下面配 email，而不是角色：display_name 恰好是 admin 时，
             admin / admin 两行不传达任何信息。角色移到头像上（描边 + 悬停）。 -->
        <div class="who">
          <span class="avatar role-{session.identity.role}" title="角色：{session.identity.role}">
            {session.identity.display_name.trim().slice(0, 1).toUpperCase()}
          </span>
          <span class="ident">
            <span class="name-line ellipsis" title={session.identity.display_name}>
              {session.identity.display_name}
            </span>
            <span class="sub ellipsis" title={session.identity.email ?? session.identity.role}>
              {session.identity.email ?? session.identity.role}
            </span>
          </span>
          <button
            class="btn-ghost btn-icon exit"
            title="退出登录"
            aria-label="退出登录"
            onclick={() => logout().then(() => location.reload())}
          >
            <Icon name="logout" />
          </button>
        </div>
      {:else if session.authDisabled}
        <span class="local" title="AI_TASK_REQUIRE_AUTH=false：回环上的单人模式，所有人都是 admin">
          本地模式 · 未启用登录
        </span>
      {/if}
    </div>
  </nav>

  <main>
    {@render children?.()}
  </main>
</div>

{#if paletteOpen}
  <CommandPalette
    onclose={() => (paletteOpen = false)}
    onnavigate={(href) => {
      paletteOpen = false;
      void goto(href);
    }}
  />
{/if}

<style>
  .shell {
    display: grid;
    grid-template-columns: 224px minmax(0, 1fr);
    min-height: 100vh;
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    padding: var(--s4) var(--s3);
    border-right: 1px solid var(--line);
    background: var(--surface-1);
    position: sticky;
    top: 0;
    height: 100vh;
    z-index: var(--z-nav);
    overflow: hidden;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: var(--s2);
    padding: 0 var(--s2) var(--s3);
    font-weight: 600;
    letter-spacing: -0.02em;
    font-size: var(--t-md);
  }
  /* 一个会呼吸的方块。这个系统的本质是"有东西在自己跑" */
  .mark {
    width: 10px;
    height: 10px;
    border-radius: 3px;
    background: var(--accent);
    box-shadow: 0 0 12px color-mix(in srgb, var(--accent) 60%, transparent);
    animation: breathe 3.5s ease-in-out infinite;
    flex: 0 0 auto;
  }
  @keyframes breathe {
    0%,
    100% {
      opacity: 1;
      transform: scale(1);
    }
    50% {
      opacity: 0.55;
      transform: scale(0.86);
    }
  }

  .search {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    padding: 0.4rem var(--s2);
    background: var(--surface-2);
    border-color: var(--line);
    color: var(--fg-faint);
    font-size: var(--t-sm);
    margin-bottom: var(--s2);
  }
  .search:hover:not(:disabled) {
    color: var(--fg-dim);
    border-color: var(--line-strong);
  }
  .search .label {
    flex: 1;
    text-align: left;
  }
  .search .kbd {
    font-size: var(--t-2xs);
    padding: 0 0.3rem;
  }

  .group {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .group + .group {
    margin-top: var(--s3);
  }
  .group-label {
    font-size: var(--t-2xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--fg-faint);
    padding: 0 var(--s2);
    margin-bottom: 2px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  ul a {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s3);
    padding: 0.44rem var(--s2);
    border-radius: var(--r2);
    color: var(--fg-dim);
    font-size: var(--t-base);
    transition:
      background var(--dur-2) var(--ease),
      color var(--dur-2) var(--ease);
  }
  ul a:hover {
    background: var(--surface-2);
    color: var(--fg);
  }
  ul a.active {
    background: var(--surface-3);
    color: var(--fg);
  }
  /* 当前页左侧一条竖线。比整块高亮更轻，也更容易扫 */
  ul a.active::before {
    content: '';
    position: absolute;
    left: -0.75rem;
    width: 2px;
    height: 1.1rem;
    border-radius: 999px;
    background: var(--accent);
  }
  ul a.active :global(svg) {
    color: var(--accent-fg);
  }
  /* 图标在 <Icon> 里，scoped 选择器打不进子组件，这里显式穿一层 */
  nav :global(svg) {
    width: 16px;
    height: 16px;
    flex: 0 0 auto;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.7;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .badge {
    margin-left: auto;
    min-width: 1.15rem;
    padding: 0 0.3rem;
    border-radius: 999px;
    background: var(--bad);
    color: #fff;
    font-size: var(--t-2xs);
    text-align: center;
    line-height: 1.15rem;
  }

  .foot {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    padding-top: var(--s3);
    border-top: 1px solid var(--line);
  }
  .foot > .btn-icon {
    align-self: flex-start;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--s2);
    padding: 0 var(--s1);
  }
  .avatar {
    width: 26px;
    height: 26px;
    flex: 0 0 auto;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-3);
    border: 1px solid var(--line-strong);
    font-size: var(--t-xs);
    font-weight: 600;
    color: var(--fg-dim);
  }
  .ident {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .name-line {
    font-size: var(--t-sm);
    color: var(--fg);
  }
  .sub {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
  }
  /* admin 有权改策略、加机器、管人，值得一眼可辨 */
  .avatar.role-admin {
    border-color: var(--accent-dim);
    color: var(--accent-fg);
    background: color-mix(in srgb, var(--accent) 12%, var(--surface-3));
  }
  .exit {
    margin-left: auto;
    flex: 0 0 auto;
  }
  .local {
    font-size: var(--t-2xs);
    color: var(--fg-faint);
    padding: 0 var(--s1);
    line-height: 1.4;
  }

  main {
    padding: var(--s5) var(--s6) var(--s7);
    max-width: 1440px;
    /* 没有这行的话，超过 1440px 的屏幕上整个应用会贴着左边，右侧空一大片 */
    margin-inline: auto;
    width: 100%;
    min-width: 0;
  }

  .topbar {
    display: none;
  }
  .scrim {
    display: none;
  }

  /* 960 以下：侧栏收成图标条 */
  @media (max-width: 960px) {
    .shell {
      grid-template-columns: 60px minmax(0, 1fr);
    }
    nav {
      padding: var(--s3) var(--s2);
      align-items: center;
    }
    nav .name,
    nav .label,
    .search .kbd,
    .group-label,
    .who .ident,
    .local {
      display: none;
    }
    .search {
      justify-content: center;
      padding: 0.4rem;
    }
    ul a {
      justify-content: center;
      padding: 0.5rem;
    }
    ul a.active::before {
      left: -0.4rem;
    }
    .badge {
      position: absolute;
      top: 2px;
      right: 2px;
      margin: 0;
      min-width: 0.9rem;
      line-height: 0.9rem;
      font-size: var(--t-2xs);
    }
    .who {
      flex-direction: column;
    }
    .exit {
      margin-left: 0;
    }
    main {
      padding: var(--s4);
    }
  }

  /* 640 以下：手机。侧栏整个收进抽屉，顶栏顶上来。
     之前这一档完全没有设计——60px 的图标条一直占着，
     本来就窄的屏幕再让掉六十分之一。 */
  @media (max-width: 640px) {
    .topbar {
      display: flex;
      align-items: center;
      gap: var(--s2);
      padding: var(--s2) var(--s3);
      padding-top: max(var(--s2), env(safe-area-inset-top, 0px));
      border-bottom: 1px solid var(--line);
      background: var(--surface-1);
      position: sticky;
      top: 0;
      z-index: var(--z-nav);
    }
    .topbar .brand {
      display: flex;
      align-items: center;
      gap: var(--s2);
      font-weight: 600;
    }

    .shell {
      /* 侧栏脱离文档流，主内容独占一列 */
      grid-template-columns: minmax(0, 1fr);
    }

    nav {
      position: fixed;
      top: 0;
      bottom: 0;
      left: 0;
      width: 240px;
      height: 100dvh;
      padding: var(--s4) var(--s3);
      align-items: stretch;
      transform: translateX(-100%);
      transition: transform var(--dur-3) var(--ease-out);
      z-index: var(--z-overlay);
      overflow-y: auto;
    }
    .drawer-open nav {
      transform: translateX(0);
      box-shadow: var(--shadow-pop);
    }

    .scrim {
      display: block;
      position: fixed;
      inset: 0;
      background: rgb(0 0 0 / 0.5);
      z-index: calc(var(--z-overlay) - 1);
      opacity: 0;
      pointer-events: none;
      transition: opacity var(--dur-3) var(--ease-out);
    }
    .drawer-open .scrim {
      opacity: 1;
      pointer-events: auto;
    }

    /* 抽屉里恢复完整的标签，图标条那套缩写在这里没必要 */
    nav .name,
    nav .label,
    .group-label,
    .who .ident,
    .local {
      display: revert;
    }
    .search {
      justify-content: flex-start;
      padding: 0.42rem 0.6rem;
    }
    ul a {
      justify-content: flex-start;
      padding: 0.45rem 0.6rem;
    }
    .badge {
      position: static;
    }
    .who {
      flex-direction: row;
    }

    main {
      padding: var(--s4) var(--s3) var(--s6);
    }
  }
</style>
