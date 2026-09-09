<script lang="ts">
  import { page } from '$app/state';
  import { api, ApiFailure } from '$api/client';
  import { triggerRun } from '$api/runs';
  import type { TaskDetail } from '$api/types/TaskDetail';
  import DagCanvas from '$lib/dag/DagCanvas.svelte';
  import { specToYaml, yamlToSpec } from '$lib/dag/spec-yaml';

  const taskId = $derived(page.params.id ?? '');

  let task = $state<TaskDetail | null>(null);
  let yaml = $state('');
  let error = $state<string | null>(null);
  let selected = $state<string | null>(null);
  let busy = $state(false);

  // YAML 是编辑时的真相来源，画布是它的实时视图。
  // 解析失败时保留上一次能画出来的 spec，免得敲到一半就白屏。
  let lastGood = $state<TaskDetail['spec'] | null>(null);
  const parsed = $derived(yamlToSpec(yaml));
  const shown = $derived(parsed.spec ?? lastGood);
  $effect(() => {
    if (parsed.spec) lastGood = parsed.spec;
  });

  const selectedNode = $derived(shown?.nodes.find((n) => n.key === selected) ?? null);

  $effect(() => {
    if (!taskId) return;
    api<TaskDetail>(`/api/v1/tasks/${taskId}`)
      .then((t) => {
        task = t;
        yaml = specToYaml(t.spec);
        lastGood = t.spec;
      })
      .catch((e) => (error = describe(e)));
  });

  function describe(e: unknown): string {
    return e instanceof ApiFailure ? `[${e.code}] ${e.message}` : String(e);
  }

  async function run() {
    busy = true;
    error = null;
    try {
      const r = await triggerRun(taskId);
      window.location.href = `/runs/${r.id}`;
    } catch (e) {
      error = describe(e);
      busy = false;
    }
  }

  function reset() {
    if (task) yaml = specToYaml(task.spec);
  }
</script>

<header>
  <div>
    <a href="/" class="muted">← 返回</a>
    <h1>{task?.name ?? '任务'}</h1>
    {#if task}
      <p class="sub">
        v{task.version_no} · {task.spec.nodes.length} 个节点
        {#if task.rules?.length}· 规则 {task.rules.join('、')}{/if}
      </p>
    {/if}
  </div>
  <button onclick={run} disabled={busy || !task}>运行</button>
</header>

{#if error}<p class="bad">{error}</p>{/if}

{#if shown}
  <DagCanvas spec={shown} onselect={(k) => (selected = k)} />
{/if}

{#if selectedNode}
  <section class="card">
    <h2>节点 <span class="mono">{selectedNode.key}</span></h2>
    <pre>{JSON.stringify(selectedNode.config, null, 2)}</pre>
  </section>
{/if}

<section class="editor">
  <header class="row">
    <h2>编排定义（YAML）</h2>
    <div>
      <button onclick={reset} disabled={!task}>还原</button>
    </div>
  </header>
  {#if parsed.error}
    <p class="bad">YAML 有问题：{parsed.error}（画布仍显示上一次能解析的版本）</p>
  {/if}
  <textarea bind:value={yaml} spellcheck="false"></textarea>
  <p class="muted">
    改动只影响画布预览。结构与语义的完整校验在后端——保存时不合法的 DAG 会被
    422 拒绝并逐条列出原因。
  </p>
</section>

<style>
  header { display: flex; align-items: flex-start; justify-content: space-between; }
  h1 { margin: 0.5rem 0 0; font-size: 1.4rem; }
  .sub { margin: 0.25rem 0 1.25rem; color: var(--muted); }
  h2 { font-size: 0.95rem; font-weight: 600; margin: 0; }
  .card {
    margin-top: 1rem; padding: 1rem 1.25rem;
    background: var(--card); border: 1px solid var(--line); border-radius: 0.75rem;
  }
  .card pre { margin: 0.5rem 0 0; overflow-x: auto; font-size: 0.8rem; }
  .editor { margin-top: 1.5rem; }
  .row { display: flex; align-items: center; justify-content: space-between; margin-bottom: 0.5rem; }
  textarea {
    width: 100%; min-height: 22rem; resize: vertical;
    font: 0.8rem/1.5 ui-monospace, SFMono-Regular, Menlo, monospace;
    padding: 0.75rem; border: 1px solid var(--line); border-radius: 0.5rem;
    background: var(--card); color: var(--fg);
  }
  .muted { font-size: 0.8rem; margin-top: 0.5rem; }
</style>
