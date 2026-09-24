<script lang="ts">
  /**
   * 应用外壳：常驻侧栏 + 命令面板 + 用户菜单。
   *
   * 常驻导航让「我在哪、能去哪」永远可见：人是从告警、从链接、从上一次的标签页里
   * 跳进来的，不是顺着一条路点进来的。
   *
   * 侧栏可以收成一条图标栏（记在本机），给宽表格和执行过程让出地方。
   */
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { session, logout } from '$lib/auth/session.svelte';
  import { resource } from '$api/resource.svelte';
  import { api } from '$api/client';
  import { listApprovals, type Approval } from '$api/models';
  import type { Overview } from '$api/types/Overview';
  import { theme, type ThemePref } from './theme.svelte';
  import CommandPalette from './CommandPalette.svelte';
  import Dropdown from './Dropdown.svelte';
  import Icon from './Icon.svelte';
  import type { IconName } from './icons';

  let { children } = $props();

  let paletteOpen = $state(false);
  /** 窄屏上侧栏收进抽屉。宽屏用不到这个状态。 */
  let drawerOpen = $state(false);

  const COLLAPSE_KEY = 'ai-task.sidebar';
  let collapsed = $state(readCollapsed());
  function readCollapsed(): boolean {
    try {
      return localStorage.getItem(COLLAPSE_KEY) === 'collapsed';
    } catch {
      return false;
    }
  }
  function toggleCollapsed() {
    collapsed = !collapsed;
    try {
      if (collapsed) localStorage.setItem(COLLAPSE_KEY, 'collapsed');
      else localStorage.removeItem(COLLAPSE_KEY);
    } catch {
      /* 存不下就只对这一次有效 */
    }
  }

  // 点了导航就把抽屉收起来——不收的话跳过去之后抽屉还盖在内容上
  $effect(() => {
    void page.url.pathname;
    drawerOpen = false;
  });

  // 和首页、/approvals、run 详情页共享同一个 key：几处订阅只产生一条请求
  const approvals = resource<Approval[]>('approvals', () => listApprovals(), { pollMs: 5000 });
  const pendingApprovals = $derived(approvals.data?.length ?? 0);
  // 执行中的数量挂在导航上：不在首页时也看得见"有东西在跑"。和首页共享 key，
  // 首页开着时按首页的 3 秒走，其它页 15 秒一次
  const overview = resource<Overview>(
    'overview',
    (signal) => api<Overview>('/api/v1/overview', { signal }),
    { pollMs: 15_000 }
  );
  const running = $derived((overview.data?.stats.running ?? 0) + (overview.data?.stats.queued ?? 0));

  interface NavItem {
    href: string;
    label: string;
    icon: IconName;
    badge?: () => { n: number; tone: 'alert' | 'info' } | null;
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
        {
          href: '/runs',
          label: '执行',
          icon: 'play',
          badge: () => (running ? { n: running, tone: 'info' } : null)
        },
        {
          href: '/approvals',
          label: '审批',
          icon: 'shield-check',
          badge: () => (pendingApprovals ? { n: pendingApprovals, tone: 'alert' } : null)
        }
      ]
    },
    {
      label: '配置',
      admin: true,
      items: [
        { href: '/hosts', label: '主机', icon: 'server' },
        { href: '/rules', label: '规则与技能', icon: 'shield' }
      ]
    },
    {
      label: '管理',
      admin: true,
      items: [
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

  const THEMES: Array<{ id: ThemePref; label: string; icon: IconName }> = [
    { id: 'system', label: '跟随系统', icon: 'monitor' },
    { id: 'light', label: '浅色', icon: 'sun' },
    { id: 'dark', label: '深色', icon: 'moon' }
  ];
  const ROLE_LABEL: Record<string, string> = { viewer: '只读', operator: '操作员', admin: '管理员' };
</script>

<svelte:window onkeydown={onKey} />

<!-- 窄屏的顶栏。宽屏上 display:none——侧栏一直在，不需要它 -->
<header class="topbar">
  <button class="btn-ghost btn-icon" aria-label="打开导航" aria-expanded={drawerOpen} onclick={() => (drawerOpen = true)}>
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

<div class="shell" class:collapsed class:drawer-open={drawerOpen}>
  <!-- 抽屉打开时点背景收起。宽屏上这块 display:none -->
  <div class="scrim" role="presentation" onclick={() => (drawerOpen = false)} aria-hidden="true"></div>
  <nav aria-label="主导航">
    <div class="brand-row">
      <a class="brand" href="/" title="ai-task">
        <span class="mark"></span>
        <span class="name">ai-task</span>
      </a>
      <button
        class="btn-ghost btn-icon btn-sm collapse"
        title={collapsed ? '展开侧栏' : '收起侧栏'}
        aria-label={collapsed ? '展开侧栏' : '收起侧栏'}
        onclick={toggleCollapsed}
      >
        <Icon name="sidebar" />
      </button>
    </div>

    <button class="search" onclick={() => (paletteOpen = true)} title="搜索任务、执行记录，或输入动作">
      <Icon name="search" />
      <span class="label">搜索或跳转</span>
      <span class="kbd">{isMac ? '⌘' : 'Ctrl'} K</span>
    </button>

    {#each groups as group (group.label ?? 'main')}
      <div class="group">
        {#if group.label}<span class="group-label">{group.label}</span>{/if}
        <ul>
          {#each group.items as item (item.href)}
            {@const badge = item.badge?.()}
            <li>
              <a
                href={item.href}
                class:active={active(item.href)}
                title={item.label}
                aria-current={active(item.href) ? 'page' : undefined}
              >
                <Icon name={item.icon} />
                <span class="label">{item.label}</span>
                {#if badge}
                  <span class="badge {badge.tone}" aria-label="{badge.n} 个">{badge.n}</span>
                {/if}
              </a>
            </li>
          {/each}
        </ul>
      </div>
    {/each}

    <div class="spacer"></div>

    <div class="foot">
      <Dropdown label="账号与主题" placement="up" align="left" triggerClass="who btn-ghost" title="账号与主题">
        {#snippet trigger()}
          {#if session.identity}
            <span class="avatar role-{session.identity.role}">
              {session.identity.display_name.trim().slice(0, 1).toUpperCase()}
            </span>
            <span class="ident">
              <span class="name-line ellipsis">{session.identity.display_name}</span>
              <span class="sub ellipsis">{ROLE_LABEL[session.identity.role] ?? session.identity.role}</span>
            </span>
          {:else}
            <span class="avatar"><Icon name="monitor" /></span>
            <span class="ident">
              <span class="name-line">本地模式</span>
              <span class="sub">未启用登录</span>
            </span>
          {/if}
          <Icon name="more" />
        {/snippet}
        {#if session.identity?.email}
          <span class="menu-head ellipsis" role="presentation">{session.identity.email}</span>
          <hr />
        {/if}
        {#each THEMES as t (t.id)}
          <button onclick={() => theme.set(t.id)}>
            <span class="item"><Icon name={t.icon} />{t.label}{#if theme.pref === t.id}<span class="current"><Icon name="check" /></span>{/if}</span>
          </button>
        {/each}
        {#if session.identity}
          <hr />
          <button onclick={() => logout().then(() => location.reload())}>
            <span class="item"><Icon name="logout" />退出登录</span>
          </button>
        {/if}
      </Dropdown>
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
    grid-template-columns: 232px minmax(0, 1fr);
    min-height: 100vh;
  }
  .shell.collapsed {
    grid-template-columns: 64px minmax(0, 1fr);
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding: var(--s3);
    border-right: 1px solid var(--line);
    background: var(--surface-1);
    position: sticky;
    top: 0;
    height: 100vh;
    z-index: var(--z-nav);
    overflow-y: auto;
    overflow-x: hidden;
  }

  .brand-row {
    display: flex;
    align-items: center;
    gap: var(--s2);
    padding: var(--s1) var(--s1) var(--s2) var(--s2);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--s2);
    font-weight: 650;
    letter-spacing: -0.02em;
    font-size: var(--t-md);
    color: var(--fg);
    min-width: 0;
  }
  .brand-row .collapse {
    margin-left: auto;
  }
  /* 一个会呼吸的方块。这个系统的本质是"有东西在自己跑" */
  .mark {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    background: linear-gradient(135deg, var(--accent), var(--accent-strong));
    box-shadow: 0 0 12px var(--accent-soft);
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
      opacity: 0.6;
      transform: scale(0.86);
    }
  }

  .search,
  :global(:root[data-theme='light']) .search {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    height: var(--h-md);
    padding: 0 var(--s2);
    background: var(--surface-2);
    border-color: var(--line);
    box-shadow: none;
    color: var(--fg-faint);
    font-size: var(--t-sm);
    font-weight: 400;
    margin-bottom: var(--s3);
  }
  .search:hover:not(:disabled) {
    background: var(--surface-2);
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
    margin-top: var(--s4);
  }
  .group-label {
    font-size: var(--t-xs);
    font-weight: 500;
    color: var(--fg-faint);
    padding: 0 var(--s2) var(--s1);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  ul a {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--s3);
    height: 2.125rem;
    padding: 0 var(--s2);
    border-radius: var(--r2);
    color: var(--fg-dim);
    font-size: var(--t-base);
    font-weight: 500;
    transition:
      background var(--dur-2) var(--ease),
      color var(--dur-2) var(--ease);
  }
  ul a:hover {
    background: var(--surface-3);
    color: var(--fg);
  }
  ul a.active {
    background: var(--accent-bg);
    color: var(--accent-fg);
  }
  /* 图标在 <Icon> 里，scoped 选择器打不进子组件，这里显式穿一层 */
  nav :global(svg) {
    width: 16px;
    height: 16px;
    flex: 0 0 auto;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .badge {
    margin-left: auto;
    min-width: 1.25rem;
    height: 1.25rem;
    padding: 0 0.35rem;
    border-radius: 999px;
    font-size: var(--t-xs);
    font-weight: 600;
    text-align: center;
    line-height: 1.25rem;
  }
  .badge.alert {
    background: var(--bad-fg);
    color: var(--surface-1);
  }
  .badge.info {
    background: var(--info-bg);
    color: var(--info-fg);
    border: 1px solid var(--info-border);
    line-height: calc(1.25rem - 2px);
  }

  .foot {
    padding-top: var(--s2);
    border-top: 1px solid var(--line);
  }
  .foot :global(.dd) {
    display: flex;
    width: 100%;
  }
  .foot :global(.who),
  :global(:root[data-theme='light']) .foot :global(.who) {
    display: flex;
    align-items: center;
    justify-content: flex-start;
    gap: var(--s2);
    width: 100%;
    height: auto;
    padding: var(--s2);
    text-align: left;
    color: var(--fg-dim);
  }
  .avatar {
    width: 28px;
    height: 28px;
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
  .avatar :global(svg) {
    width: 14px;
    height: 14px;
  }
  /* admin 有权改策略、加机器、管人，值得一眼可辨 */
  .avatar.role-admin {
    border-color: var(--accent-border);
    color: var(--accent-fg);
    background: var(--accent-bg);
  }
  .ident {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    flex: 1;
  }
  .name-line {
    font-size: var(--t-sm);
    font-weight: 500;
    color: var(--fg);
  }
  .sub {
    font-size: var(--t-xs);
    font-weight: 400;
    color: var(--fg-faint);
  }
  .menu-head {
    display: block;
    padding: 0.35rem 0.6rem;
    font-size: var(--t-xs);
    color: var(--fg-faint);
    max-width: 14rem;
  }
  .item {
    display: flex;
    align-items: center;
    gap: var(--s2);
    width: 100%;
  }
  .item :global(svg) {
    width: 15px;
    height: 15px;
    color: var(--fg-faint);
  }
  .current {
    margin-left: auto;
    display: inline-flex;
  }
  .current :global(svg) {
    color: var(--accent-fg);
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

  /* 收起的图标条（宽屏上用户自己选的，960 以下自动） */
  .shell.collapsed nav {
    align-items: center;
    padding: var(--s3) var(--s2);
  }
  .shell.collapsed .name,
  .shell.collapsed .label,
  .shell.collapsed .search .kbd,
  .shell.collapsed .group-label,
  .shell.collapsed .ident,
  .shell.collapsed .foot :global(.who > svg:last-child) {
    display: none;
  }
  .shell.collapsed .brand-row {
    flex-direction: column;
    padding: var(--s1) 0 var(--s2);
  }
  .shell.collapsed .brand-row .collapse {
    margin-left: 0;
  }
  .shell.collapsed .search {
    justify-content: center;
    padding: 0;
    width: var(--h-md);
  }
  .shell.collapsed ul a {
    justify-content: center;
    width: 2.5rem;
    padding: 0;
  }
  .shell.collapsed .badge {
    position: absolute;
    top: 1px;
    right: 1px;
    margin: 0;
    min-width: 1rem;
    height: 1rem;
    line-height: 1rem;
    font-size: var(--t-2xs);
    padding: 0 0.2rem;
  }
  .shell.collapsed .foot :global(.who) {
    justify-content: center;
    padding: var(--s1);
  }

  @media (max-width: 960px) {
    .shell {
      grid-template-columns: 64px minmax(0, 1fr);
    }
    nav {
      align-items: center;
      padding: var(--s3) var(--s2);
    }
    .name,
    .label,
    .search .kbd,
    .group-label,
    .ident,
    .brand-row .collapse,
    .foot :global(.who > svg:last-child) {
      display: none;
    }
    .search {
      justify-content: center;
      padding: 0;
      width: var(--h-md);
    }
    ul a {
      justify-content: center;
      width: 2.5rem;
      padding: 0;
    }
    .badge {
      position: absolute;
      top: 1px;
      right: 1px;
      margin: 0;
      min-width: 1rem;
      height: 1rem;
      line-height: 1rem;
      font-size: var(--t-2xs);
      padding: 0 0.2rem;
    }
    .foot :global(.who) {
      justify-content: center;
    }
    main {
      padding: var(--s4);
    }
  }

  /* 640 以下：手机。侧栏整个收进抽屉，顶栏顶上来 */
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
    .shell,
    .shell.collapsed {
      grid-template-columns: minmax(0, 1fr);
    }
    nav,
    .shell.collapsed nav {
      position: fixed;
      top: 0;
      bottom: 0;
      left: 0;
      width: 248px;
      height: 100dvh;
      padding: var(--s4) var(--s3);
      align-items: stretch;
      transform: translateX(-100%);
      transition: transform var(--dur-3) var(--ease-out);
      z-index: var(--z-overlay);
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
    .ident,
    .foot :global(.who > svg:last-child),
    .shell.collapsed nav .name,
    .shell.collapsed nav .label,
    .shell.collapsed .group-label,
    .shell.collapsed .ident {
      display: revert;
    }
    .search,
    .shell.collapsed .search {
      justify-content: flex-start;
      width: 100%;
      padding: 0 var(--s2);
    }
    ul a,
    .shell.collapsed ul a {
      justify-content: flex-start;
      width: auto;
      padding: 0 var(--s2);
    }
    .badge,
    .shell.collapsed .badge {
      position: static;
      margin-left: auto;
    }
    .brand-row .collapse {
      display: none;
    }
    main {
      padding: var(--s4) var(--s3) var(--s6);
    }
  }
</style>
