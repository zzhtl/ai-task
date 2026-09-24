// 带游标翻页的列表怎么和轮询共存。

/**
 * 轮询只拉第一页，**合并**进已经翻出来的列表：已有的行按 id 换成新数据，
 * 新出现的插到最前，已经翻出来的后几页原样保留。
 *
 * 整表替换会把人翻到的位置冲掉——任务详情页曾经就是这样：点了"更早的记录"，
 * 五秒后又被弹回最近 20 条。
 */
export function mergeFirstPage<T extends { id: string }>(current: T[], fresh: T[]): T[] {
  const byId = new Map(fresh.map((x) => [x.id, x]));
  const known = new Set(current.map((x) => x.id));
  const added = fresh.filter((x) => !known.has(x.id));
  return [...added, ...current.map((x) => byId.get(x.id) ?? x)];
}
