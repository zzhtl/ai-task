<script lang="ts">
  /**
   * 审计流水：谁在什么时候对什么做了什么。
   *
   * 这个系统能 SSH 到任意机器执行命令、能改策略、能批准 AI 落地修改——
   * "这条 deny 是什么时候被谁停掉的"必须查得到。后端一直在记，这里把它摆出来。
   */
  import { describeError } from '$api/client';
  import { pollWhileVisible } from '$api/resource.svelte';
  import { listAudit, listUsers, type AuditItem, type User } from '$api/models';
  import { session } from '$lib/auth/session.svelte';
  import PageHeader from '$lib/ui/PageHeader.svelte';
  import Empty from '$lib/ui/Empty.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { ago, stamp } from '$lib/ui/format';

  let items = $state<AuditItem[]>([]);
  let users = $state<User[]>([]);
  let error = $state<string | null>(null);
  let loaded = $state(false);
  let query = $state('');
  let kind = $state('');

  const ACTION: Record<string, string> = {
    'task.create': '创建任务',
    'task.update': '修改任务',
    'task.delete': '删除任务',
    'run.trigger': '触发执行',
    'run.cancel': '取消执行',
    'run.delete': '删除执行记录',
    'schedule.create': '添加定时',
    'schedule.update': '修改定时',
    'schedule.delete': '删除定时',
    'schedule.set_enabled': '切换定时启停',
    'approval.decide': '审批决策',
    'rule.create': '新增规则',
    'rule.update': '修改规则',
    'rule.delete': '删除规则',
    'rule.enabled': '切换规则启停',
    'skill.create': '导入技能',
    'host.create': '添加主机',
    'host.update': '修改主机',
    'host.delete': '删除主机',
    'user.create': '创建用户',
    'user.update': '修改用户资料',
    'user.delete': '删除用户',
    'user.set_role': '改角色',
    'user.set_disabled': '停用/恢复用户',
    'user.revoke_sessions': '踢下线'
  };
  const KIND: Record<string, string> = {
    task: '任务',
    run: '执行',
    schedule: '定时',
    approval: '审批',
    rule: '规则',
    skill: '技能',
    host: '主机',
    user: '用户'
  };

  const PAGE = 50;
  let cursor = $state<string | null>(null);
  let loadingMore = $state(false);

  /**
   * 界面上的动作名是中文（"创建主机"），而库里存的是 `host.create`。
   * 这张映射表只存在于前端，服务端搜不到它——所以搜索词命中哪些中文名，
   * 就把对应的动作码一起送过去，而不是把整张表复制一份到后端。
   */
  function matchedActions(q: string): string[] {
    const needle = q.trim().toLowerCase();
    if (!needle) return [];
    return Object.entries(ACTION)
      .filter(([, label]) => label.toLowerCase().includes(needle))
      .map(([code]) => code);
  }

  const filter = $derived({
    q: query.trim() || undefined,
    targetKind: kind || undefined,
    actions: matchedActions(query)
  });

  async function load() {
    try {
      const [page, u] = await Promise.all([
        listAudit({ ...filter, limit: PAGE }),
        listUsers().catch(() => [] as User[])
      ]);
      items = page.items;
      cursor = page.next_cursor ?? null;
      users = u;
      error = null;
    } catch (e) {
      error = describeError(e);
    } finally {
      loaded = true;
    }
  }

  /** 已经翻过页了。翻过之后就停掉自动刷新——见下面 pollWhileVisible 处的说明。 */
  let paged = $state(false);

  /** 翻下一页。追加而不是替换，保住已经看过的和滚动位置。 */
  async function loadMore() {
    if (!cursor || loadingMore) return;
    loadingMore = true;
    try {
      const page = await listAudit({ ...filter, cursor, limit: PAGE });
      items = [...items, ...page.items];
      cursor = page.next_cursor ?? null;
      paged = true;
    } catch (e) {
      error = describeError(e);
    } finally {
      loadingMore = false;
    }
  }

  $effect(() => {
    if (!session.can('admin')) return;
    // 筛选条件变了就重新从第一页拉。**筛选在服务端做**——
    // 之前是硬拉最近 300 条回来自己过滤，于是"最近 300 条里恰好没有"
    // 和"根本没发生过"在界面上长得一模一样，而审计恰恰不能这样糊弄。
    void filter;
    paged = false;
    void load();
    // **翻过页之后就不再自动刷新。** load() 是整份替换，而用户往下翻了几页
    // 正说明他在查历史——十秒后把他弹回第一页，比不刷新难受得多。
    // 审计是 append-only 的，翻页期间错过的新记录换个筛选或刷新就回来了。
    return pollWhileVisible(() => {
      if (!paged) void load();
    }, 10_000);
  });

  const userNames = $derived(new Map(users.map((u) => [u.id, u.display_name])));
  const actorName = (id: string | null) =>
    id === null ? '系统' : (userNames.get(id) ?? id.slice(0, 8));

  const href = (item: AuditItem): string | null => {
    if (item.target_kind === 'run') return `/runs/${item.target_id}`;
    if (item.target_kind === 'task') return `/tasks/${item.target_id}`;
    return null;
  };

  /** 可选的对象类型。取自 KIND 表而不是当前这一页——
      按类型筛选的选项不该因为这一页里恰好没有主机就消失。 */
  const kinds = Object.keys(KIND).sort();

  // 筛选已经在服务端做完了，这里直接用
  const shown = $derived(items);
  const hasFilter = $derived(!!query.trim() || !!kind);

  const pretty = (v: unknown) => JSON.stringify(v, null, 2);
</script>

<PageHeader title="审计" help="谁在什么时候对什么做了什么。每一条都带 request id，能和服务端的访问日志对上。">
  {#snippet sub()}
    <span>已加载 {items.length} 条{cursor ? '，还有更早的' : ''}</span>
  {/snippet}
  {#snippet actions()}
    <select bind:value={kind}>
      <option value="">所有对象</option>
      {#each kinds as k (k)}<option value={k}>{KIND[k] ?? k}</option>{/each}
    </select>
    <input class="search" bind:value={query} type="search" placeholder="按人、动作、id 找" />
  {/snippet}
</PageHeader>

{#if !session.can('admin')}
  <div class="callout danger">需要管理员权限：审计里是"谁做了什么"。</div>
{:else}
  {#if error}<div class="banner">{error}</div>{/if}

  {#if !loaded}
    <div class="card"><Loading rows={6} /></div>
  {:else if shown.length}
    <div class="card flush">
      <table>
        <thead>
          <tr><th>时间</th><th>谁</th><th>做了什么</th><th>对象</th><th>详情</th></tr>
        </thead>
        <tbody>
          {#each shown as item (item.id)}
            <tr>
              <td class="faint nowrap" title={stamp(item.ts)}>{ago(item.ts)}</td>
              <td class="nowrap">
                <span class:system={item.actor === null}>{actorName(item.actor)}</span>
              </td>
              <td class="nowrap">
                <span class="action">{ACTION[item.action] ?? item.action}</span>
                <span class="mono faint small">{item.action}</span>
              </td>
              <td class="nowrap">
                <span class="tag">{KIND[item.target_kind] ?? item.target_kind}</span>
                {#if href(item)}
                  <a class="mono" href={href(item)}>{item.target_id.slice(0, 8)}</a>
                {:else}
                  <span class="mono faint" title={item.target_id}>{item.target_id.slice(0, 8)}</span>
                {/if}
              </td>
              <td class="detail">
                {#if item.after !== null && item.after !== undefined}
                  <details>
                    <summary class="faint small">
                      {pretty(item.after).replace(/\s+/g, ' ').slice(0, 80)}
                    </summary>
                    {#if item.before !== null && item.before !== undefined}
                      <span class="lbl">之前</span>
                      <pre>{pretty(item.before)}</pre>
                    {/if}
                    <span class="lbl">之后</span>
                    <pre>{pretty(item.after)}</pre>
                    {#if item.request_id}
                      <span class="lbl">request id</span>
                      <code>{item.request_id}</code>
                    {/if}
                  </details>
                {:else}
                  <span class="faint">—</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    {#if cursor}
      <div class="more">
        <button class="btn-sm" onclick={loadMore} disabled={loadingMore}>
          {loadingMore ? '加载中…' : '加载更早的记录'}
        </button>
      </div>
    {/if}
  {:else if hasFilter}
    <Empty title="没有匹配的记录" hint="换个关键词或对象类型。这次查的是全部历史，不只是最近几页。" />
  {:else}
    <Empty title="还没有审计记录" hint="建任务、触发执行、改策略这些动作发生后会出现在这里。" />
  {/if}
{/if}

<style>
  .more {
    display: flex;
    justify-content: center;
    padding: var(--s3) 0;
  }
  .system {
    color: var(--fg-faint);
    font-style: italic;
  }
  .action {
    margin-right: var(--s2);
  }
  .detail {
    max-width: 40ch;
  }
  .detail summary {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40ch;
  }
  .detail details[open] summary {
    white-space: normal;
  }
  .lbl {
    display: block;
    margin-top: var(--s2);
    font-size: var(--t-2xs);
    color: var(--fg-faint);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  pre {
    margin: 2px 0 0;
    padding: var(--s2);
    background: var(--surface-2);
    border-radius: var(--r1);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 14rem;
    overflow: auto;
    font-size: var(--t-xs);
  }
</style>
