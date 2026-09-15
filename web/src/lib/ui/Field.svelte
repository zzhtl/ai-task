<script lang="ts">
  /**
   * 一个表单字段：标签 + 控件 + 提示 + **错误**。
   *
   * 存在的理由只有最后一项。后端在 422 里一次给全 `details[]`，每条都带 `field`，
   * 但界面此前只把它们拼成一行塞进页头的 banner——人读到"名称：不能为空；
   * 端口：超出范围"之后，还得自己回到表单里找是哪两个框。
   *
   * 控件用 snippet 传进来，参数里带着已经连好的 id / aria-invalid / aria-describedby，
   * 所以调用方写的仍然是原生 `<input>`，样式继续走 app.css 的全局规则——
   * 不为了包一层而再造一套 Input/Select/Textarea。
   */
  let {
    label,
    hint,
    error,
    wide = false,
    control
  }: {
    label: string;
    hint?: string;
    /** 这个字段的错误。来自 `fieldErrors(e)[name]`。 */
    error?: string;
    /** 在 .form-grid 里占满一整行。 */
    wide?: boolean;
    control: import('svelte').Snippet<
      [{ id: string; 'aria-invalid': 'true' | undefined; 'aria-describedby': string | undefined }]
    >;
  } = $props();

  const id = $props.id();
  const hintId = `${id}-hint`;
  const errId = `${id}-err`;

  // 读屏要能把"哪里错了"和输入框连起来；错误优先，没错误时念提示
  const describedBy = $derived(error ? errId : hint ? hintId : undefined);
</script>

<label class="field" class:wide class:invalid={!!error} for={id}>
  <span class="label-text">{label}</span>
  {@render control({ id, 'aria-invalid': error ? 'true' : undefined, 'aria-describedby': describedBy })}
  {#if error}
    <span class="field-error" id={errId}>{error}</span>
  {:else if hint}
    <span class="hint" id={hintId}>{hint}</span>
  {/if}
</label>

<style>
  /* 出错的框自己要看得出来，不能只靠底下那行字 */
  .field.invalid :global(input),
  .field.invalid :global(select),
  .field.invalid :global(textarea) {
    border-color: var(--bad);
  }
  .field.invalid :global(input:focus-visible),
  .field.invalid :global(select:focus-visible),
  .field.invalid :global(textarea:focus-visible) {
    border-color: var(--bad);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--bad) 18%, transparent);
  }
</style>
