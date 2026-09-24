<script lang="ts">
  /**
   * 执行详情左侧的步骤导航：每一步的状态、耗时、花费，一眼看出卡在哪、错在哪。
   * 点一下跳到过程里对应的那一段。
   */
  import StatusBadge from '$lib/ui/StatusBadge.svelte';
  import { duration, moneyMicros } from '$lib/ui/format';
  import type { NavStep } from './process';

  let {
    steps,
    onselect
  }: {
    steps: NavStep[];
    onselect: (key: string) => void;
  } = $props();

  const BAD = new Set(['failed', 'timed_out', 'budget_exceeded', 'resource_exceeded', 'cancelled', 'interrupted']);
</script>

<nav class="steps" aria-label="步骤">
  <span class="title">步骤</span>
  <ol>
    {#each steps as step, i (step.key)}
      <li>
        <button
          type="button"
          class:bad={BAD.has(step.status)}
          class:live={step.status === 'running' || step.status === 'awaiting_approval'}
          onclick={() => onselect(step.key)}
          title={step.name}
        >
          <StatusBadge status={step.status} variant="dot" />
          <span class="text">
            <span class="name ellipsis">{i + 1}. {step.name}</span>
            <span class="meta-line">
              {#if step.startedAt}{duration(step.startedAt, step.finishedAt)}{:else if step.status === 'skipped'}跳过{:else}未开始{/if}
              {#if step.costMicros > 0}· {moneyMicros(step.costMicros)}{/if}
            </span>
          </span>
        </button>
      </li>
    {/each}
  </ol>
</nav>

<style>
  .steps {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .title {
    font-size: var(--t-xs);
    font-weight: 500;
    color: var(--fg-faint);
    padding: 0 var(--s2);
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  button,
  :global(:root[data-theme='light']) .steps button {
    display: flex;
    align-items: flex-start;
    justify-content: flex-start;
    gap: var(--s2);
    width: 100%;
    height: auto;
    padding: var(--s2);
    border: 1px solid transparent;
    border-radius: var(--r2);
    background: transparent;
    box-shadow: none;
    color: var(--fg);
    text-align: left;
    font-weight: 400;
  }
  button:hover:not(:disabled) {
    background: var(--surface-3);
    border-color: transparent;
  }
  button.bad {
    border-color: var(--bad-border);
    background: var(--bad-bg);
  }
  button.live {
    border-color: var(--info-border);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .name {
    font-size: var(--t-sm);
    font-weight: 500;
  }
  .meta-line {
    font-size: var(--t-xs);
    color: var(--fg-faint);
  }
</style>
