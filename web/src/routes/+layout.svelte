<script lang="ts">
  import '../app.css';
  import LoginGate from '$lib/auth/LoginGate.svelte';
  import Shell from '$lib/ui/Shell.svelte';
  import Toasts from '$lib/ui/Toasts.svelte';

  let { children } = $props();
</script>

<svelte:head><title>ai-task</title></svelte:head>

<LoginGate>
  <Shell>
    {@render children?.()}
  </Shell>
</LoginGate>
<Toasts />
