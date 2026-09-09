// DagSpec ↔ YAML。
//
// YAML 是编辑时的真相来源，画布是它的实时视图。反方向（在画布上拖出一个节点）
// 是结构化编辑，那是另一个量级的工作量，现在没做。

import { parse, stringify } from 'yaml';
import type { DagSpec } from '$api/types/DagSpec';

export function specToYaml(spec: DagSpec): string {
  // 省掉空数组和 null，让手写的 YAML 看起来跟人写的一样
  return stringify(prune(spec), { indent: 2, lineWidth: 100 });
}

export interface ParseResult {
  spec?: DagSpec;
  error?: string;
}

export function yamlToSpec(text: string): ParseResult {
  if (!text.trim()) return { error: 'YAML 是空的' };
  let value: unknown;
  try {
    value = parse(text);
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return { error: '顶层必须是一个对象，包含 nodes 与 edges' };
  }
  const spec = value as Partial<DagSpec>;
  if (!Array.isArray(spec.nodes) || spec.nodes.length === 0) {
    return { error: 'nodes 必须是非空数组' };
  }
  // 只做够画图的最小校验；结构与语义的完整校验在后端（保存时会 422）
  for (const [i, node] of spec.nodes.entries()) {
    if (!node || typeof node !== 'object' || typeof node.key !== 'string') {
      return { error: `nodes[${i}] 缺少 key` };
    }
    if (!node.config || typeof node.config.kind !== 'string') {
      return { error: `节点 ${node.key} 缺少 config.kind` };
    }
  }
  return { spec: { edges: [], ...spec } as DagSpec };
}

/** 递归去掉 null / undefined / 空数组 / 空对象。 */
function prune(value: unknown): unknown {
  if (Array.isArray(value)) {
    const items = value.map(prune).filter((v) => v !== undefined);
    return items.length > 0 ? items : undefined;
  }
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value)) {
      const pruned = prune(v);
      if (pruned !== undefined) out[k] = pruned;
    }
    return Object.keys(out).length > 0 ? out : undefined;
  }
  return value === null ? undefined : value;
}
