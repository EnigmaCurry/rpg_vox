<script>
  import { scenesState, renameProject } from '../lib/scenes.svelte.js';

  // Everything on this page edits fields on the currently-loaded project.
  // When no project is selected we bail early with an inline prompt.
  const project = $derived(
    scenesState.selectedProjectId
      ? scenesState.projects.find((p) => p.id === scenesState.selectedProjectId) ?? null
      : null,
  );

  // Local draft so the input can hold typed text without racing with the
  // reactive scene state. `editing` guards the sync-from-project effect so
  // remote/state changes don't stomp what the user is typing.
  let nameDraft = $state('');
  let editing = $state(false);

  $effect(() => {
    if (!editing && project) nameDraft = project.name;
  });

  function commitName() {
    editing = false;
    if (!project) return;
    const name = nameDraft.trim() || 'Untitled project';
    if (name !== project.name) renameProject(project.id, name);
    nameDraft = name;
  }

  function onNameKey(e) {
    if (e.key === 'Enter') { e.preventDefault(); e.currentTarget.blur(); }
    else if (e.key === 'Escape') {
      e.preventDefault();
      if (project) nameDraft = project.name;
      editing = false;
      e.currentTarget.blur();
    }
  }
</script>

<section class="wrap">
  <header class="head">
    <h1>Settings</h1>
    {#if project}
      <p class="sub">Per-project settings for <strong>{project.name}</strong>.</p>
    {:else}
      <p class="sub">Load a project to edit its settings.</p>
    {/if}
  </header>

  {#if project}
    <div class="field">
      <label for="project-name">Project name</label>
      <input
        id="project-name"
        type="text"
        bind:value={nameDraft}
        onfocus={() => (editing = true)}
        onblur={commitName}
        onkeydown={onNameKey}
      />
    </div>
  {:else}
    <div class="empty">No project loaded.</div>
  {/if}
</section>

<style>
  .wrap { display: flex; flex-direction: column; gap: 18px; }
  .head h1 { margin: 0 0 4px; font-size: 20px; font-weight: 600; color: var(--text); }
  .sub { margin: 0; color: var(--muted); font-size: 13px; }
  .empty {
    padding: 24px;
    color: var(--muted);
    font-size: 13px;
    text-align: center;
    border: 1px dashed var(--border);
    border-radius: 8px;
  }
  .field { display: flex; flex-direction: column; gap: 6px; }
  .field label { font-size: 12px; color: var(--muted); font-weight: 600; }
  .field input {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
    font-family: inherit;
  }
  .field input:focus { outline: none; border-color: var(--accent); }
</style>
