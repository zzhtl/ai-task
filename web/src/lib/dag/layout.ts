// DAG 自动布局。
//
// 不引 dagre：这里只需要"按拓扑层级分列、层内平均分行"，三十行就够了。
// 引一个通用图布局库要多带一百多 KB，还得处理它的坐标系约定。

import type { DagSpec } from '$api/types/DagSpec';
import type { NodeSpec } from '$api/types/NodeSpec';

export interface Placed {
  key: string;
  spec: NodeSpec;
  /** 拓扑层级，从 0 开始。 */
  rank: number;
  /** 层内序号。 */
  row: number;
  x: number;
  y: number;
}

const COLUMN_WIDTH = 260;
const ROW_HEIGHT = 110;

/**
 * 按最长路径分层。
 *
 * 用最长路径而不是最短：一个节点应当排在**所有**上游的右边，
 * 用最短路径的话菱形拓扑里的长边会从右往左画回去，很难看懂。
 */
export function layout(spec: DagSpec): { nodes: Placed[]; hasCycle: boolean } {
  const byKey = new Map(spec.nodes.map((n) => [n.key, n]));
  const incoming = new Map<string, string[]>();
  const outgoing = new Map<string, string[]>();

  for (const node of spec.nodes) {
    incoming.set(node.key, []);
    outgoing.set(node.key, []);
  }
  for (const edge of spec.edges ?? []) {
    // 悬空的边直接忽略：后端会在保存时拒掉，前端只是画不出来而已
    if (!byKey.has(edge.from) || !byKey.has(edge.to)) continue;
    incoming.get(edge.to)?.push(edge.from);
    outgoing.get(edge.from)?.push(edge.to);
  }

  // Kahn 算法求拓扑序，顺带算最长路径层级
  const indegree = new Map([...incoming].map(([k, v]) => [k, v.length]));
  const rank = new Map<string, number>(spec.nodes.map((n) => [n.key, 0]));
  const queue = spec.nodes.filter((n) => (indegree.get(n.key) ?? 0) === 0).map((n) => n.key);
  let visited = 0;

  while (queue.length > 0) {
    const key = queue.shift() as string;
    visited += 1;
    for (const next of outgoing.get(key) ?? []) {
      rank.set(next, Math.max(rank.get(next) ?? 0, (rank.get(key) ?? 0) + 1));
      const left = (indegree.get(next) ?? 1) - 1;
      indegree.set(next, left);
      if (left === 0) queue.push(next);
    }
  }

  // 有环时后端会拒掉，但编辑过程中出现环是常态，画布不能因此白屏
  const hasCycle = visited < spec.nodes.length;

  const rows = new Map<number, number>();
  const nodes: Placed[] = spec.nodes.map((node) => {
    const r = rank.get(node.key) ?? 0;
    const row = rows.get(r) ?? 0;
    rows.set(r, row + 1);
    return {
      key: node.key,
      spec: node,
      rank: r,
      row,
      x: r * COLUMN_WIDTH,
      y: row * ROW_HEIGHT
    };
  });

  return { nodes, hasCycle };
}

/** 节点类型 → 展示用的短标签与配色变量名。 */
export function nodeKindLabel(node: NodeSpec): { label: string; tone: string } {
  switch (node.config.kind) {
    case 'ai':
      return { label: 'AI', tone: 'ai' };
    case 'shell':
      return { label: 'Shell', tone: 'shell' };
    case 'assert':
      return { label: '断言', tone: 'assert' };
    case 'approval':
      return { label: '审批', tone: 'approval' };
    case 'map':
      return { label: '扇出', tone: 'map' };
    default:
      // 后端加了新节点类型时不能白屏
      return { label: (node.config as { kind: string }).kind, tone: 'unknown' };
  }
}

/** 边条件 → 展示用的标签。`on_success` 是默认值，不标注免得图上全是字。 */
export function edgeLabel(when: { op: string } | undefined): string {
  switch (when?.op) {
    case 'on_failure':
      return '失败时';
    case 'always':
      return '总是';
    case 'expr':
      return '条件';
    default:
      return '';
  }
}
