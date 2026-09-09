<script lang="ts">
  // DAG 画布。
  //
  // 可以叠加每个节点的实时状态：同一张图既是编排视图，也是执行视图。
  // 这样看一个正在跑的 run 时，不用在"图"和"事件流"之间来回对照。

  import {
    SvelteFlow,
    Background,
    Controls,
    Position,
    type Edge,
    type Node
  } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import type { DagSpec } from '$api/types/DagSpec';
  import type { NodeStatus } from '$api/types/NodeStatus';
  import { edgeLabel, layout, nodeKindLabel } from './layout';

  interface Props {
    spec: DagSpec;
    /** 节点 key → 实时状态。给了就叠加显示。 */
    status?: Record<string, NodeStatus>;
    /** 节点 key → map 展开出的实例数。 */
    expanded?: Record<string, number>;
    onselect?: (key: string) => void;
  }

  const { spec, status = {}, expanded = {}, onselect }: Props = $props();

  const placed = $derived(layout(spec));

  const nodes = $derived<Node[]>(
    placed.nodes.map((p) => {
      const { label, tone } = nodeKindLabel(p.spec);
      const state = status[p.key];
      const count = expanded[p.key];
      return {
        id: p.key,
        position: { x: p.x, y: p.y },
        data: {
          label: `${label} · ${p.spec.name ?? p.key}${count ? ` ×${count}` : ''}`
        },
        // 布局是横向的，句柄也要在左右两侧；用默认的上下会让连线绕一圈
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
        type: 'default',
        class: `dag-node tone-${tone}${state ? ` state-${state}` : ''}`
      } satisfies Node;
    })
  );

  const edges = $derived<Edge[]>(
    (spec.edges ?? []).map((e, i) => ({
      id: `e${i}`,
      source: e.from,
      target: e.to,
      label: edgeLabel(e.when),
      animated: status[e.from] === 'running',
      class: `dag-edge when-${e.when?.op ?? 'on_success'}`
    }))
  );
</script>

{#if placed.hasCycle}
  <p class="bad">图中存在环，无法完整布局。保存时后端会拒绝这份编排。</p>
{/if}

<div class="canvas">
  <SvelteFlow
    {nodes}
    {edges}
    fitView
    nodesDraggable={false}
    nodesConnectable={false}
    onnodeclick={({ node }) => onselect?.(node.id)}
  >
    <Background />
    <Controls showLock={false} />
  </SvelteFlow>
</div>

<style>
  .canvas {
    height: 22rem;
    border: 1px solid var(--line);
    border-radius: 0.75rem;
    overflow: hidden;
    background: var(--card);
  }
  /* SvelteFlow 的节点渲染在组件内部，只能用 :global 上色 */
  :global(.dag-node) {
    border-radius: 0.5rem;
    border: 1px solid var(--line);
    background: var(--card);
    color: var(--fg);
    font-size: 0.8rem;
    padding: 0.5rem 0.75rem;
    min-width: 8rem;
  }
  :global(.dag-node.tone-ai) { border-left: 3px solid #38bdf8; }
  :global(.dag-node.tone-map) { border-left: 3px solid #a78bfa; }
  :global(.dag-node.tone-assert) { border-left: 3px solid #fbbf24; }
  :global(.dag-node.tone-approval) { border-left: 3px solid #fb923c; }
  :global(.dag-node.tone-shell) { border-left: 3px solid #94a3b8; }
  :global(.dag-node.state-running) { box-shadow: 0 0 0 2px #38bdf8; }
  :global(.dag-node.state-succeeded) { border-color: var(--ok); }
  :global(.dag-node.state-failed) { border-color: var(--bad); }
  :global(.dag-node.state-skipped),
  :global(.dag-node.state-cancelled) { opacity: 0.45; }
  :global(.dag-edge.when-on_failure .svelte-flow__edge-path) { stroke: var(--bad); }
  :global(.svelte-flow__edge-text) { font-size: 0.7rem; fill: var(--muted); }
</style>
