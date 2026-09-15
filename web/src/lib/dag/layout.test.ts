// 分层布局。图是人手写的编排，规模小，但错了一眼就能看出来——
// 所以这里断言的是那些"看起来不对"的具体形态。

import { describe, expect, test } from 'bun:test';
import { CyclicDag, layout, NODE_H, NODE_W } from './layout';

const node = (key: string, kind = 'ai') => ({ key, label: key, kind });
const edge = (from: string, to: string) => ({ from, to });

/** key -> 层号 */
const layers = (l: ReturnType<typeof layout>) =>
  Object.fromEntries(l.nodes.map((n) => [n.key, n.layer]));

describe('分层', () => {
  test('线性链一节一层', () => {
    const l = layout({
      nodes: [node('a'), node('b'), node('c')],
      edges: [edge('a', 'b'), edge('b', 'c')]
    });
    expect(layers(l)).toEqual({ a: 0, b: 1, c: 2 });
  });

  test('并行分支排在同一层', () => {
    //   a
    //  / \
    // b   c
    //  \ /
    //   d
    const l = layout({
      nodes: [node('a'), node('b'), node('c'), node('d')],
      edges: [edge('a', 'b'), edge('a', 'c'), edge('b', 'd'), edge('c', 'd')]
    });
    const at = layers(l);
    expect(at.b).toBe(at.c);
    expect(at.a).toBe(0);
    expect(at.d).toBe(2);
  });

  test('走最长路径，不是最短', () => {
    // a → b → c 和 a → c 同时存在时，c 必须排在 b 后面，
    // 否则 a→c 那条边会往回连，图上看着就是错的
    const l = layout({
      nodes: [node('a'), node('b'), node('c')],
      edges: [edge('a', 'b'), edge('b', 'c'), edge('a', 'c')]
    });
    expect(layers(l)).toEqual({ a: 0, b: 1, c: 2 });
  });

  test('没有边的节点各自成岛，都在第 0 层', () => {
    const l = layout({ nodes: [node('a'), node('b')], edges: [] });
    expect(layers(l)).toEqual({ a: 0, b: 0 });
    expect(l.layers).toBe(1);
  });

  test('有环时报错，不是死循环', () => {
    expect(() =>
      layout({
        nodes: [node('a'), node('b')],
        edges: [edge('a', 'b'), edge('b', 'a')]
      })
    ).toThrow(CyclicDag);
  });

  test('指向不存在节点的边被忽略，不会把整张图弄崩', () => {
    const l = layout({ nodes: [node('a')], edges: [edge('a', '不存在')] });
    expect(l.nodes).toHaveLength(1);
    expect(l.edges).toHaveLength(0);
  });
});

describe('坐标', () => {
  test('同层不重叠', () => {
    const l = layout({
      nodes: [node('a'), node('b'), node('c'), node('d')],
      edges: [edge('a', 'b'), edge('a', 'c'), edge('a', 'd')]
    });
    const row = l.nodes.filter((n) => n.layer === 1).sort((x, y) => x.x - y.x);
    for (const [i, n] of row.entries()) {
      if (i === 0) continue;
      expect(n.x).toBeGreaterThanOrEqual(row[i - 1]!.x + NODE_W);
    }
  });

  test('下一层在上一层下面', () => {
    const l = layout({
      nodes: [node('a'), node('b')],
      edges: [edge('a', 'b')]
    });
    const a = l.nodes.find((n) => n.key === 'a')!;
    const b = l.nodes.find((n) => n.key === 'b')!;
    expect(b.y).toBeGreaterThanOrEqual(a.y + NODE_H);
  });

  test('画布装得下所有节点', () => {
    const l = layout({
      nodes: [node('a'), node('b'), node('c')],
      edges: [edge('a', 'b'), edge('a', 'c')]
    });
    for (const n of l.nodes) {
      expect(n.x + NODE_W).toBeLessThanOrEqual(l.width);
      expect(n.y + NODE_H).toBeLessThanOrEqual(l.height);
    }
  });
});

describe('连线', () => {
  test('每条边都有能画的 path', () => {
    const l = layout({
      nodes: [node('a'), node('b')],
      edges: [edge('a', 'b')]
    });
    expect(l.edges[0]!.path).toMatch(/^M [\d.]+ [\d.]+ C /);
  });

  test('跨多层的边绕开中间节点，不走直线', () => {
    // a → b → c，外加一条 a → c。后者跨两层，直着连会从 b 的框里穿过去
    const l = layout({
      nodes: [node('a'), node('b'), node('c')],
      edges: [edge('a', 'b'), edge('b', 'c'), edge('a', 'c')]
    });
    const skip = l.edges.find((e) => e.from === 'a' && e.to === 'c')!;
    const direct = l.edges.find((e) => e.from === 'a' && e.to === 'b')!;
    // 绕行的控制点会落在节点列之外
    const controlXs = [...skip.path.matchAll(/C ([\d.-]+) [\d.-]+, ([\d.-]+)/g)].flatMap((m) => [
      Number(m[1]),
      Number(m[2])
    ]);
    const b = l.nodes.find((n) => n.key === 'b')!;
    expect(controlXs.some((x) => x < b.x || x > b.x + NODE_W)).toBe(true);
    expect(skip.path).not.toBe(direct.path);
  });
});

describe('确定性', () => {
  test('同一份编排每次画出来一样', () => {
    const input = {
      nodes: [node('a'), node('b'), node('c'), node('d'), node('e')],
      edges: [edge('a', 'b'), edge('a', 'c'), edge('b', 'd'), edge('c', 'd'), edge('d', 'e')]
    };
    expect(JSON.stringify(layout(input))).toBe(JSON.stringify(layout(input)));
  });

  test('钻石图不该出现交叉', () => {
    // a 分出 b/c 再汇到 d：b 和 c 的左右顺序不该让两条汇合线交叉
    const l = layout({
      nodes: [node('a'), node('b'), node('c'), node('d')],
      edges: [edge('a', 'b'), edge('a', 'c'), edge('b', 'd'), edge('c', 'd')]
    });
    const row = l.nodes.filter((n) => n.layer === 1);
    expect(row).toHaveLength(2);
    expect(row[0]!.x).not.toBe(row[1]!.x);
  });
});
