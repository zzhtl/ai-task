<script lang="ts">
  /**
   * 一次执行的结果。
   *
   * 人来看执行详情，第一件想知道的是"结论是什么"。以前它藏在最后一步
   * 折起来的 JSON 里，得先找到那一步、再点开、再在一堆引号里找。
   *
   * - 字符串：按正文显示；
   * - 只有一个字符串字段的对象（`{text}`、`{result}` 这类）：按正文显示那个字段；
   * - 扁平对象：键值表；
   * - 其它：格式化的 JSON。
   */
  import CopyButton from '$lib/ui/CopyButton.svelte';

  let { output }: { output: unknown } = $props();

  type View =
    | { kind: 'prose'; text: string; field: string | null }
    | { kind: 'pairs'; pairs: Array<[string, string]> }
    | { kind: 'json'; text: string };

  const isPrimitive = (v: unknown) =>
    v === null || ['string', 'number', 'boolean'].includes(typeof v);

  const view = $derived.by((): View => {
    if (typeof output === 'string') return { kind: 'prose', text: output, field: null };
    if (output && typeof output === 'object' && !Array.isArray(output)) {
      const entries = Object.entries(output as Record<string, unknown>);
      if (entries.length === 1 && typeof entries[0][1] === 'string') {
        return { kind: 'prose', text: entries[0][1] as string, field: entries[0][0] };
      }
      if (entries.length > 0 && entries.length <= 12 && entries.every(([, v]) => isPrimitive(v))) {
        return {
          kind: 'pairs',
          pairs: entries.map(([k, v]) => [k, typeof v === 'string' ? v : JSON.stringify(v)])
        };
      }
    }
    return { kind: 'json', text: JSON.stringify(output, null, 2) };
  });

  const copyText = $derived(typeof output === 'string' ? output : JSON.stringify(output, null, 2));
</script>

<div class="output">
  <span class="copy"><CopyButton text={copyText} label="复制结果" /></span>
  {#if view.kind === 'prose'}
    {#if view.field}<span class="field mono">{view.field}</span>{/if}
    <p class="prose">{view.text}</p>
  {:else if view.kind === 'pairs'}
    <dl class="kv">
      {#each view.pairs as [k, v] (k)}
        <dt class="mono">{k}</dt>
        <!-- 空串不留白：一片空白分不清是"没输出"还是"没加载出来" -->
        <dd class:blank={v === ''}>{v === '' ? '（空）' : v}</dd>
      {/each}
    </dl>
  {:else}
    <pre class="mono">{view.text}</pre>
  {/if}
</div>

<style>
  .output {
    position: relative;
    min-width: 0;
  }
  .copy {
    position: absolute;
    top: 0;
    right: 0;
  }
  .field {
    display: block;
    font-size: var(--t-xs);
    color: var(--fg-faint);
    margin-bottom: var(--s1);
  }
  .prose {
    margin: 0;
    padding-right: 2.5rem;
    font-size: var(--t-base);
    line-height: 1.75;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 28rem;
    overflow-y: auto;
  }
  .kv {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: var(--s1) var(--s4);
    margin: 0;
    padding-right: 2.5rem;
    font-size: var(--t-sm);
  }
  .kv dt {
    color: var(--fg-faint);
    font-size: var(--t-xs);
    padding-top: 2px;
  }
  .kv dd {
    margin: 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .kv dd.blank {
    color: var(--fg-faint);
  }
  pre {
    margin: 0;
    padding: var(--s3);
    padding-right: 2.5rem;
    border: 1px solid var(--line);
    border-radius: var(--r2);
    background: var(--surface-2);
    font-size: var(--t-xs);
    line-height: 1.6;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 24rem;
    overflow-y: auto;
  }
</style>
