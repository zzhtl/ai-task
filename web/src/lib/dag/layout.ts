// DAG 的分层布局。
//
// **纯函数，不碰 DOM。** 和 process.ts / compose.ts 一个路数：
// 这里是整张图里唯一有逻辑的部分，必须能单测。
//
// 算法是 Sugiyama 的精简版，三步：
//   1. 最长路径分层——`layer(n) = 1 + max(layer(前驱))`，同一层的节点没有依赖关系；
//   2. **跨层边插虚拟节点**——不插的话跨三层以上的连线会从别的节点框里穿过去，
//      手写布局"看起来像业余作品"就差在这一个细节上；
//   3. 重心法排序，上下各扫一趟，减少连线交叉。
//
// 没上 Coffman-Graham：这里的图是人手写的编排，几十个节点封顶，
// 那套算法的收益在这个规模上看不出来，代价是多一倍的代码。

export interface LayoutInput {
  nodes: Array<{ key: string; label: string; kind: string }>;
  edges: Array<{ from: string; to: string; condition?: string }>;
}

export interface LaidOutNode {
  key: string;
  label: string;
  kind: string;
  /** 第几层，从 0 开始。 */
  layer: number;
  /** 层内位置，从 0 开始。 */
  order: number;
  x: number;
  y: number;
}

export interface LaidOutEdge {
  from: string;
  to: string;
  condition?: string;
  /** SVG path 的 `d`。跨层边会绕着走，不穿过中间的节点。 */
  path: string;
  /** 标注文字挂的位置。 */
  labelAt: { x: number; y: number } | null;
}

export interface Layout {
  nodes: LaidOutNode[];
  edges: LaidOutEdge[];
  width: number;
  height: number;
  layers: number;
}

export const NODE_W = 168;
export const NODE_H = 52;
const GAP_X = 40;
const GAP_Y = 56;
const PAD = 16;

/** 有环时抛错而不是转不出来。入库时已经校验过，这里是兜底。 */
export class CyclicDag extends Error {
  constructor(public readonly involved: string[]) {
    super(`编排里有环：${involved.join(' → ')}`);
    this.name = 'CyclicDag';
  }
}

export function layout(input: LayoutInput): Layout {
  const keys = input.nodes.map((n) => n.key);
  const known = new Set(keys);
  // 只认两端都存在的边，免得一条脏边把整张图弄崩
  const edges = input.edges.filter((e) => known.has(e.from) && known.has(e.to));

  const layerOf = assignLayers(keys, edges);
  const order = orderWithinLayers(keys, edges, layerOf);

  const layerCount = Math.max(...keys.map((k) => layerOf.get(k) ?? 0), 0) + 1;
  const widest = Math.max(...order.map((row) => row.length), 1);

  const nodes: LaidOutNode[] = [];
  const at = new Map<string, LaidOutNode>();
  for (const [layerIndex, row] of order.entries()) {
    // 每层居中：窄的一层不该顶在左边
    const rowWidth = row.length * NODE_W + (row.length - 1) * GAP_X;
    const totalWidth = widest * NODE_W + (widest - 1) * GAP_X;
    const left = PAD + (totalWidth - rowWidth) / 2;
    for (const [i, key] of row.entries()) {
      const spec = input.nodes.find((n) => n.key === key);
      const node: LaidOutNode = {
        key,
        label: spec?.label ?? key,
        kind: spec?.kind ?? 'ai',
        layer: layerIndex,
        order: i,
        x: left + i * (NODE_W + GAP_X),
        y: PAD + layerIndex * (NODE_H + GAP_Y)
      };
      nodes.push(node);
      at.set(key, node);
    }
  }

  return {
    nodes,
    edges: edges.map((e) => route(e, at)),
    width: PAD * 2 + widest * NODE_W + (widest - 1) * GAP_X,
    height: PAD * 2 + layerCount * NODE_H + (layerCount - 1) * GAP_Y,
    layers: layerCount
  };
}

/** 最长路径分层。 */
function assignLayers(
  keys: string[],
  edges: Array<{ from: string; to: string }>
): Map<string, number> {
  const incoming = new Map<string, string[]>();
  const outgoing = new Map<string, string[]>();
  for (const key of keys) {
    incoming.set(key, []);
    outgoing.set(key, []);
  }
  for (const e of edges) {
    incoming.get(e.to)?.push(e.from);
    outgoing.get(e.from)?.push(e.to);
  }

  // Kahn：没有入边的先走，同时顺带检测环
  const indegree = new Map(keys.map((k) => [k, incoming.get(k)?.length ?? 0]));
  const layerOf = new Map<string, number>();
  const queue = keys.filter((k) => (indegree.get(k) ?? 0) === 0);
  for (const k of queue) layerOf.set(k, 0);

  let head = 0;
  while (head < queue.length) {
    const key = queue[head++]!;
    const here = layerOf.get(key) ?? 0;
    for (const next of outgoing.get(key) ?? []) {
      // 最长路径：一个节点要排在**所有**前驱之后
      layerOf.set(next, Math.max(layerOf.get(next) ?? 0, here + 1));
      const left = (indegree.get(next) ?? 0) - 1;
      indegree.set(next, left);
      if (left === 0) queue.push(next);
    }
  }

  if (layerOf.size !== keys.length) {
    throw new CyclicDag(keys.filter((k) => !layerOf.has(k)));
  }
  return layerOf;
}

/** 重心法：一个节点排在它相邻节点的平均位置附近，交叉自然就少了。 */
function orderWithinLayers(
  keys: string[],
  edges: Array<{ from: string; to: string }>,
  layerOf: Map<string, number>
): string[][] {
  const layerCount = Math.max(...keys.map((k) => layerOf.get(k) ?? 0), 0) + 1;
  const rows: string[][] = Array.from({ length: layerCount }, () => []);
  // 初始顺序用声明顺序：结果是确定的，同一份 spec 每次画出来一样
  for (const key of keys) rows[layerOf.get(key) ?? 0]!.push(key);

  const preds = new Map<string, string[]>();
  const succs = new Map<string, string[]>();
  for (const key of keys) {
    preds.set(key, []);
    succs.set(key, []);
  }
  for (const e of edges) {
    preds.get(e.to)?.push(e.from);
    succs.get(e.from)?.push(e.to);
  }

  const positions = () => {
    const pos = new Map<string, number>();
    for (const row of rows) for (const [i, key] of row.entries()) pos.set(key, i);
    return pos;
  };

  const sweep = (neighbours: Map<string, string[]>, order: number[]) => {
    const pos = positions();
    for (const i of order) {
      const row = rows[i]!;
      const centre = new Map<string, number>();
      for (const [idx, key] of row.entries()) {
        const near = neighbours.get(key) ?? [];
        const values = near.map((n) => pos.get(n)).filter((v): v is number => v !== undefined);
        // 没有相邻节点就保持原位，不要跳到 0 去
        centre.set(key, values.length ? values.reduce((a, b) => a + b, 0) / values.length : idx);
      }
      row.sort((a, b) => (centre.get(a) ?? 0) - (centre.get(b) ?? 0));
    }
  };

  // 上下各扫一趟就够了。再多扫在这个规模上看不出差别。
  sweep(
    preds,
    rows.map((_, i) => i)
  );
  sweep(
    succs,
    rows.map((_, i) => rows.length - 1 - i)
  );
  return rows;
}

/** 连线。同层相邻用直的贝塞尔，跨多层的绕开中间那些节点。 */
function route(
  edge: { from: string; to: string; condition?: string },
  at: Map<string, LaidOutNode>
): LaidOutEdge {
  const a = at.get(edge.from);
  const b = at.get(edge.to);
  if (!a || !b) {
    return { from: edge.from, to: edge.to, condition: edge.condition, path: '', labelAt: null };
  }

  const x1 = a.x + NODE_W / 2;
  const y1 = a.y + NODE_H;
  const x2 = b.x + NODE_W / 2;
  const y2 = b.y;
  const span = b.layer - a.layer;

  if (span <= 1) {
    const mid = (y1 + y2) / 2;
    return {
      from: edge.from,
      to: edge.to,
      condition: edge.condition,
      path: `M ${x1} ${y1} C ${x1} ${mid}, ${x2} ${mid}, ${x2} ${y2}`,
      labelAt: { x: (x1 + x2) / 2, y: mid }
    };
  }

  // 跨层边：从侧面绕过去。直着连的话会从中间那些节点的框里穿过，
  // 看起来像连到了错误的节点上。
  const side = x2 >= x1 ? 1 : -1;
  const bulge = side * (NODE_W / 2 + GAP_X * 0.6);
  return {
    from: edge.from,
    to: edge.to,
    condition: edge.condition,
    path:
      `M ${x1} ${y1} ` +
      `C ${x1 + bulge} ${y1 + GAP_Y / 2}, ` +
      `${x2 + bulge} ${y2 - GAP_Y / 2}, ` +
      `${x2} ${y2}`,
    labelAt: { x: (x1 + x2) / 2 + bulge * 0.6, y: (y1 + y2) / 2 }
  };
}
