<script lang="ts">
  /**
   * 一个技能的全文：看、改、删。
   *
   * 改是"另存一个新版本"：旧版本原样保留，历史 run 用的是哪一版能对得上。
   * 打开时拿到的那一版 `id` 就是 ETag——期间别人改过，保存会被 412 挡住，
   * 这时只能重新加载再改，不能拿旧内容去盖。
   */
  import { ApiFailure, describeError, fieldErrors } from '$api/client';
  import { deleteSkill, getSkill, updateSkill } from '$api/models';
  import type { SkillDetail } from '$api/types/SkillDetail';
  import Modal from '$lib/ui/Modal.svelte';
  import Field from '$lib/ui/Field.svelte';
  import Icon from '$lib/ui/Icon.svelte';
  import Loading from '$lib/ui/Loading.svelte';
  import { toast, toastError } from '$lib/ui/toast.svelte';
  import { ago, stamp } from '$lib/ui/format';
  import { filesBody, nextVersion, type FileRow } from './skills';

  let {
    name,
    canEdit,
    onclose,
    onchanged
  }: {
    /** 要看的技能；`null` 表示关着。 */
    name: string | null;
    /** operator 及以上能改能删。 */
    canEdit: boolean;
    onclose: () => void;
    /** 改了或删了：列表要重新拉。 */
    onchanged: () => void;
  } = $props();

  let detail = $state<SkillDetail | null>(null);
  let loadError = $state<string | null>(null);
  let editing = $state(false);
  let version = $state('');
  let description = $state('');
  let body = $state('');
  let files = $state<FileRow[]>([]);
  let errors = $state<Record<string, string>>({});
  let saving = $state(false);
  /** 打开之后别人改过：只能重新加载。 */
  let conflict = $state(false);
  let confirmingDelete = $state(false);

  async function load(skill: string) {
    detail = null;
    loadError = null;
    editing = false;
    conflict = false;
    confirmingDelete = false;
    errors = {};
    try {
      detail = await getSkill(skill);
    } catch (e) {
      loadError = describeError(e);
    }
  }

  $effect(() => {
    if (name) void load(name);
  });

  function startEdit() {
    if (!detail) return;
    version = nextVersion(detail.version);
    description = detail.description;
    body = detail.body;
    files = Object.entries(detail.files).map(([path, content]) => ({ path, content }));
    errors = {};
    conflict = false;
    editing = true;
  }

  async function save() {
    if (!detail) return;
    const built = filesBody(files);
    if ('error' in built) {
      errors = { files: built.error };
      return;
    }
    saving = true;
    errors = {};
    try {
      const next = await updateSkill(
        detail.name,
        { version: version.trim(), description, body, files: built.files },
        detail.id
      );
      toast(next.id === detail.id ? '内容没有变化，没有新建版本' : `已保存为版本 ${next.version}`);
      detail = next;
      editing = false;
      onchanged();
    } catch (e) {
      if (e instanceof ApiFailure && e.status === 412) {
        conflict = true;
      } else {
        errors = fieldErrors(e);
        if (!Object.keys(errors).length) toastError(describeError(e));
      }
    } finally {
      saving = false;
    }
  }

  async function remove() {
    if (!detail) return;
    saving = true;
    try {
      await deleteSkill(detail.name);
      toast(`已删除技能 ${detail.name}`);
      onchanged();
      onclose();
    } catch (e) {
      // 409：刚好有任务挂上了它。消息里列着任务名
      toastError(describeError(e));
      confirmingDelete = false;
    } finally {
      saving = false;
    }
  }

  const fileEntries = $derived(detail ? Object.entries(detail.files) : []);
</script>

<Modal open={name !== null} title={name ? `技能 ${name}` : ''} size="lg" {onclose}>
  {#if loadError}
    <p class="banner">{loadError}</p>
  {:else if !detail}
    <Loading rows={6} />
  {:else if !editing}
    <p class="meta faint small">
      <span>版本 <b class="fg">{detail.version}</b>（共 {detail.versions} 版）</span>
      <span title={stamp(detail.created_at)}>{ago(detail.created_at)}更新</span>
      <span class="mono" title="正文和附件的指纹，不含描述">{detail.content_hash.slice(0, 12)}</span>
    </p>
    <section>
      <h3>描述</h3>
      <p>{detail.description}</p>
      <p class="faint small">渐进式披露时模型只看得到这一句，据此决定要不要读正文。</p>
    </section>
    <section>
      <h3>在用的任务</h3>
      {#if detail.used_by.length}
        <p class="tags">{#each detail.used_by as t (t)}<span class="tag">{t}</span>{/each}</p>
      {:else}
        <p class="faint small">没有任务在用。</p>
      {/if}
    </section>
    <section>
      <h3>正文 <span class="faint small">SKILL.md</span></h3>
      <pre class="text">{detail.body}</pre>
    </section>
    {#if fileEntries.length}
      <section>
        <h3>附件</h3>
        {#each fileEntries as [path, content] (path)}
          <details>
            <summary class="mono small">{path}</summary>
            <pre class="text">{content}</pre>
          </details>
        {/each}
      </section>
    {/if}
  {:else}
    {#if conflict}
      <div class="callout warn">
        这个技能在你打开之后被别人改过，不能拿旧内容去覆盖。
        <button class="btn-sm" onclick={() => name && load(name)}>重新加载</button>
        （你这边的修改会丢掉，先复制出来）
      </div>
    {/if}
    <div class="form-grid">
      <Field label="新版本号" hint="每次保存都是一个新版本，旧版本保留" error={errors.version}>
        {#snippet control(p)}
          <input {...p} bind:value={version} spellcheck="false" />
        {/snippet}
      </Field>
      <Field label="描述" hint="模型只凭这一句决定要不要加载它" error={errors.description} wide>
        {#snippet control(p)}
          <input {...p} bind:value={description} />
        {/snippet}
      </Field>
      <Field label="正文（SKILL.md）" error={errors.body} wide>
        {#snippet control(p)}
          <textarea {...p} bind:value={body} rows="14" class="mono" spellcheck="false"></textarea>
        {/snippet}
      </Field>
      <div class="wide files">
        <span class="label">附件<span class="faint">放在技能目录下的相对路径，如 ref/checklist.md</span></span>
        {#each files as file, i (i)}
          <div class="file">
            <input bind:value={file.path} class="mono" placeholder="ref/notes.md" spellcheck="false" aria-label="附件路径" />
            <button
              class="btn-ghost btn-sm btn-icon danger"
              title="去掉这个附件"
              aria-label="去掉附件 {file.path}"
              onclick={() => (files = files.filter((_, at) => at !== i))}
            >
              <Icon name="close" />
            </button>
            <textarea bind:value={file.content} rows="4" class="mono" spellcheck="false" aria-label="附件内容"></textarea>
          </div>
        {/each}
        <div>
          <button class="btn-sm" onclick={() => (files = [...files, { path: '', content: '' }])}>
            <Icon name="plus" /> 添加附件
          </button>
        </div>
        {#if errors.files}<span class="field-error">{errors.files}</span>{/if}
      </div>
    </div>
  {/if}

  {#snippet footer()}
    {#if detail && !editing}
      {#if canEdit}
        {#if confirmingDelete}
          <span class="left small">删掉全部 {detail.versions} 个版本，拿不回来。</span>
          <button class="btn-ghost" onclick={() => (confirmingDelete = false)} disabled={saving}>取消</button>
          <button class="btn-danger" onclick={remove} disabled={saving}>删除</button>
        {:else}
          <button
            class="btn-ghost danger left"
            disabled={detail.used_by.length > 0}
            title={detail.used_by.length ? `还有任务在用：${detail.used_by.join('、')}` : '删除这个技能'}
            onclick={() => (confirmingDelete = true)}
          >
            删除
          </button>
          <button class="btn-ghost" onclick={onclose}>关闭</button>
          <button class="btn-primary" onclick={startEdit}>编辑</button>
        {/if}
      {:else}
        <button class="btn-ghost" onclick={onclose}>关闭</button>
      {/if}
    {:else if detail && editing}
      <button class="btn-ghost" onclick={() => (editing = false)} disabled={saving}>放弃修改</button>
      <button class="btn-primary" onclick={save} disabled={saving || conflict || !version.trim()}>
        保存为新版本
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .meta {
    display: flex;
    gap: var(--s3);
    flex-wrap: wrap;
    margin: 0 0 var(--s3);
  }
  .fg {
    color: var(--fg);
  }
  section + section {
    margin-top: var(--s4);
  }
  h3 {
    margin: 0 0 var(--s1);
    font-size: var(--t-sm);
    font-weight: 600;
  }
  section p {
    margin: 0;
  }
  .tags {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .text {
    margin: 0;
    padding: var(--s3);
    max-height: 22rem;
    overflow: auto;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r2);
    font-size: var(--t-xs);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  details + details {
    margin-top: var(--s2);
  }
  summary {
    cursor: pointer;
  }
  .files {
    display: flex;
    flex-direction: column;
    gap: var(--s2);
  }
  .files .label {
    display: flex;
    gap: var(--s2);
    font-size: var(--t-xs);
    color: var(--fg-dim);
  }
  .file {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: var(--s1) var(--s2);
  }
  .file textarea {
    grid-column: 1 / -1;
  }
  .left {
    margin-right: auto;
  }
</style>
