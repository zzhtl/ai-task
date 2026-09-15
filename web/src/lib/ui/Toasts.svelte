<script lang="ts">
  import { toasts } from './toast.svelte';
  import Icon from './Icon.svelte';
</script>

<div class="toasts" aria-live="polite">
  {#each toasts.items as t (t.id)}
    <div class="toast {t.kind}" role="status">
      <span class="mark"></span>
      <span class="text">{t.text}</span>
      <button class="btn-ghost btn-sm btn-icon" aria-label="关闭" onclick={() => toasts.dismiss(t.id)}>
        <Icon name="close" />
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: var(--s4);
    bottom: var(--s4);
    z-index: var(--z-toast);
    display: flex;
    flex-direction: column;
    gap: var(--s2);
    width: min(380px, calc(100vw - 2rem));
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: flex-start;
    gap: var(--s2);
    padding: var(--s3) var(--s3) var(--s3) var(--s4);
    border-radius: var(--r3);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    box-shadow: var(--shadow-pop);
    font-size: var(--t-base);
    animation: rise var(--dur-3) var(--ease-out);
  }
  .mark {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    margin-top: 0.4rem;
    flex: 0 0 auto;
    background: var(--accent);
  }
  .toast.ok .mark {
    background: var(--ok);
  }
  .toast.bad .mark {
    background: var(--bad);
  }
  .toast.bad {
    border-color: color-mix(in srgb, var(--bad) 40%, var(--line-strong));
  }
  .text {
    flex: 1;
    overflow-wrap: anywhere;
    line-height: 1.5;
  }
</style>
