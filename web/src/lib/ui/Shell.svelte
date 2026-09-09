<script lang="ts">
  /**
   * 应用外壳：常驻侧栏 + 顶栏 + 命令面板。
   *
   * 每个页面各自放一个「← 返回」是 1998 年的做法：它假设用户是顺着一条路进来的，
   * 而实际上人是从告警、从链接、从上一次的标签页里跳进来的。常驻导航让「我在哪、
   * 能去哪」永远可见。
   */
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { session, logout } from '$lib/auth/session.svelte';
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

  const NAV: NavItem[] = [
    { href: '/', label: '概览', icon: 'M3 12h4l3-8 4 16 3-8h4' },
    { href: '/tasks', label: '任务', icon: 'M4 6h16M4 12h16M4 18h10' },
    { href: '/runs', label: '执行', icon: 'M5 3l14 9-14 9V3z' },
    {
      href: '/approvals',
      label: '审批',
      icon: 'M9 12l2 2 4-4M12 3l8 4v6c0 4-3.5 7-8 8-4.5-1-8-4-8-8V7z',
      badge: () => pendingApprovals
    },
    { href: '/hosts', label: '主机', icon: 'M4 6h16v5H4zM4 14h16v5H4zM8 8.5h.01M8 16.5h.01', admin: true },
    { href: '/rules', label: '规则', icon: 'M12 3l8 4v6c0 4-3.5 7-8 8-4.5-1-8-4-8-8V7z', admin: true },
    { href: '/users', label: '用户', icon: 'M4 20c0-3 3.6-5 8-5s8 2 8 5M12 11a4 4 0 100-8 4 4 0 000 8', admin: true }
  ];

  const visible = $derived(NAV.filter((n) => !n.admin || session.can('admin')));

  function active(href: string): boolean {
    return href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);
  }

  $effect(() => {
    const load = () =>
      fetch('/api/v1/approvals', { headers: { accept: 'application/json' } })
        .then((r) => (r.ok ? r.json() : { items: [] }))
        .then((p: { items: unknown[] }) => (pendingApprovals = p.items.length))
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
</script>

<svelte:window onkeydown={onKey} />

<div class="shell">
  <nav>
    <a class="brand" href="/">
      <span class="mark"></span>
      <span class="name">ai-task</span>
    </a>

    <ul>
      {#each visible as item (item.href)}
        <li>
          <a href={item.href} class:active={active(item.href)}>
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d={item.icon} />
            </svg>
            <span>{item.label}</span>
            {#if item.badge?.()}
              <span class="badge">{item.badge()}</span>
            {/if}
          </a>
        </li>
      {/each}
    </ul>

    <div class="spacer"></div>

    {#if session.identity}
      <!-- 名字下面配角色，遇上 display_name 恰好就是角色名（admin / admin）
           时会像坏了：同一个词写两遍不传达任何信息。第二行改成 email
           ——它才是团队里真正能区分人的东西；角色移到头像上（描边 + 悬停）。 -->
      <div class="who">
        <span class="avatar role-{session.identity.role}" title="角色：{session.identity.role}">
          {session.identity.display_name.trim().slice(0, 1).toUpperCase()}
        </span>
        <span class="ident">
          <span class="name-line" title={session.identity.display_name}>
            {session.identity.display_name}
          </span>
          <span class="sub" title={session.identity.email ?? session.identity.role}>
            {session.identity.email ?? session.identity.role}
          </span>
        </span>
        <button
          class="btn-ghost btn-sm exit"
          title="退出登录"
          aria-label="退出登录"
          onclick={() => logout().then(() => location.reload())}
        >
          <svg viewBox="0 0 24 24"><path d="M9 21H5a2 2 0 01-2-2V5a2 2 0 012-2h4M16 17l5-5-5-5M21 12H9" /></svg>
        </button>
      </div>
    {/if}
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
    grid-template-columns: 216px 1fr;
    min-height: 100vh;
  }
  @media (max-width: 820px) {
    .shell {
      grid-template-columns: 60px 1fr;
    }
    nav .name,
    nav li span,
    .who {
      display: none;
    }
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: var(--s1);
    padding: var(--s4) var(--s3);
    border-right: 1px solid var(--line);
    background: var(--surface-1);
    position: sticky;
    top: 0;
    height: 100vh;
    z-index: var(--z-nav);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: var(--s2);
    padding: 0 var(--s2) var(--s4);
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  /* 一个会呼吸的方块。这个系统的本质是"有东西在自己跑" */
  .mark {
    width: 10px;
    height: 10px;
    border-radius: 3px;
    background: var(--accent);
    box-shadow: 0 0 12px color-mix(in srgb, var(--accent) 60%, transparent);
    animation: breathe 3.5s ease-in-out infinite;
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

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  ul a {
    display: flex;
    align-items: center;
    gap: var(--s3);
    padding: 0.42rem var(--s2);
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
    margin-left: -0.75rem;
    width: 2px;
    height: 1.1rem;
    border-radius: 999px;
    background: var(--accent);
  }
  svg {
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

  .who {
    display: flex;
    align-items: center;
    gap: var(--s2);
    padding: var(--s3) var(--s1) 0;
    margin-top: var(--s2);
    border-top: 1px solid var(--line);
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
  .name-line,
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    padding: 0.25rem;
    flex: 0 0 auto;
  }
  .exit svg {
    width: 15px;
    height: 15px;
  }

  main {
    padding: var(--s5) var(--s6);
    max-width: 1400px;
    width: 100%;
  }
  @media (max-width: 820px) {
    main {
      padding: var(--s4);
    }
  }
</style>
