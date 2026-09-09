<script lang="ts">
  import LoginGate from '$lib/auth/LoginGate.svelte';

  let { children } = $props();
</script>

<svelte:head><title>ai-task</title></svelte:head>

<main>
  <LoginGate>
    {@render children?.()}
  </LoginGate>
</main>

<style>
  :global(:root) {
    --bg: #f8fafc;
    --fg: #0f172a;
    --muted: #64748b;
    --line: #e2e8f0;
    --card: #ffffff;
    --ok: #16a34a;
    --bad: #dc2626;
    color-scheme: light dark;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #0b1120;
      --fg: #e2e8f0;
      --muted: #94a3b8;
      --line: #1e293b;
      --card: #111827;
      --ok: #4ade80;
      --bad: #f87171;
    }
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--fg);
    font: 14px/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", "PingFang SC",
      "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
  }
  main {
    max-width: 60rem;
    margin: 0 auto;
    padding: 3rem 1.5rem;
  }
  :global(.mono) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  :global(.muted) { color: var(--muted); }
  :global(.bad) { color: var(--bad); }
  :global(.tag) {
    display: inline-block; margin-left: 0.4rem; padding: 0 0.35rem;
    border: 1px solid var(--line); border-radius: 0.25rem;
    font-size: 0.7rem; color: var(--muted);
  }
  :global(.status) { font-weight: 600; }
  :global(.status.succeeded) { color: var(--ok); }
  :global(.status.failed), :global(.status.timed_out),
  :global(.status.budget_exceeded), :global(.status.resource_exceeded) { color: var(--bad); }
  :global(.status.cancelled) { color: var(--muted); }
  :global(button), :global(.btn) {
    font: inherit; padding: 0.3rem 0.75rem; border: 1px solid var(--line);
    border-radius: 0.4rem; background: transparent; color: inherit;
    cursor: pointer; text-decoration: none; white-space: nowrap;
  }
  :global(button:hover), :global(.btn:hover) { border-color: var(--muted); }
  :global(button:disabled) { opacity: 0.5; cursor: default; }
  :global(a) { color: inherit; }
</style>
