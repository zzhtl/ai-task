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
  import { listApprovals } from '$api/models';
  import { theme } from './theme.svelte';
  import CommandPalette from './CommandPalette.svelte';

  let { children } = $props();

  let pendingApprovals = $state(0);
  let paletteOpen = $state(false);

  interface NavItem {
    href: string;
    label: string;
    icon: string;
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
        { href: '/', label: '概览', icon: 'M3 12h4l3-8 4 16 3-8h4' },
        { href: '/tasks', label: '任务', icon: 'M4 6h16M4 12h16M4 18h10' },
        { href: '/runs', label: '执行', icon: 'M5 3l14 9-14 9V3z' },
        {
          href: '/approvals',
          label: '审批',
          icon: 'M9 12l2 2 4-4M12 3l8 4v6c0 4-3.5 7-8 8-4.5-1-8-4-8-8V7z',
          badge: () => pendingApprovals
        }
      ]
    },
    {
      label: '管理',
      admin: true,
      items: [
        { href: '/hosts', label: '主机', icon: 'M4 6h16v5H4zM4 14h16v5H4zM8 8.5h.01M8 16.5h.01' },
        { href: '/rules', label: '规则', icon: 'M12 3l8 4v6c0 4-3.5 7-8 8-4.5-1-8-4-8-8V7z' },
        { href: '/users', label: '用户', icon: 'M4 20c0-3 3.6-5 8-5s8 2 8 5M12 11a4 4 0 100-8 4 4 0 000 8' },
        { href: '/audit', label: '审计', icon: 'M8 4h8l4 4v12H4V4h4zM8 12h8M8 16h5' }
      ]
    }
  ];

  const groups = $derived(GROUPS.filter((g) => !g.admin || session.can('admin')));

  function active(href: string): boolean {
    return href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
  }

  $effect(() => {
    const load = () =>
      listApprovals()
        .then((items) => (pendingApprovals = items.length))
        .catch(() => {});
    void load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });

  function onKey(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      paletteOpen = !paletteOpen;
    }
  }

  const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);
</script>

<svelte:window onkeydown={onKey} />

<div class="shell">
  <nav aria-label="主导航">
    <a class="brand" href="/">
      <span class="mark"></span>
      <span class="name">ai-task</span>
    </a>

    <button class="search" onclick={() => (paletteOpen = true)} title="搜索任务、执行记录">
      <svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7" /><path d="M20 20l-3.5-3.5" /></svg>
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
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d={item.icon} />
                </svg>
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
          <svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></svg>
        {:else}
          <svg viewBox="0 0 24 24"><path d="M21 12.8A9 9 0 1111.2 3a7 7 0 009.8 9.8z" /></svg>
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
            <svg viewBox="0 0 24 24"><path d="M9 21H5a2 2 0 01-2-2V5a2 2 0 012-2h4M16 17l5-5-5-5M21 12H9" /></svg>
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
    font-size: 0.95rem;
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
    font-size: 0.82rem;
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
    font-size: 0.66rem;
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
    font-size: 0.68rem;
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
    font-size: 0.875rem;
    transition:
      background 0.12s ease,
      color 0.12s ease;
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
  ul a.active svg {
    color: var(--accent-fg);
  }
  nav svg {
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
    font-size: 0.7rem;
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
    font-size: 0.72rem;
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
    font-size: 0.8rem;
    color: var(--fg);
  }
  .sub {
    font-size: 0.68rem;
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
    font-size: 0.7rem;
    color: var(--fg-faint);
    padding: 0 var(--s1);
    line-height: 1.4;
  }

  main {
    padding: var(--s5) var(--s6) var(--s7);
    max-width: 1440px;
    width: 100%;
    min-width: 0;
  }

  @media (max-width: 820px) {
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
      font-size: 0.6rem;
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
</style>
