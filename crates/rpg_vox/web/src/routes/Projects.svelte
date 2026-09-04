<script>
  import {
    scenesState,
    createProject,
    selectProject,
    renameProject,
    deleteProject,
  } from '../lib/scenes.svelte.js';

  let newName = $state('');

  // Two-click delete confirm keyed by project id.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  let deleteErr = $state('');
  const DELETE_CONFIRM_MS = 2500;

  // Inline rename.
  let renamingId = $state(null);
  let renameDraft = $state('');
  let renameEl = $state(null);

  function onCreate(ev) {
    ev.preventDefault();
    const name = newName.trim();
    if (!name) return;
    createProject(name);
    newName = '';
  }

  function onSelect(id) {
    if (renamingId) return;
    selectProject(id);
  }

  async function armDelete(id, ev) {
    ev.stopPropagation();
    if (deleteArmedFor === id) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      const { failed } = (await deleteProject(id)) ?? { failed: 0 };
      if (failed > 0) {
        deleteErr = `${failed} clip${failed === 1 ? '' : 's'} failed to delete on server`;
        setTimeout(() => { deleteErr = ''; }, 4000);
      }
      return;
    }
    deleteArmedFor = id;
    if (deleteArmTimer) clearTimeout(deleteArmTimer);
    deleteArmTimer = setTimeout(() => {
      deleteArmedFor = null;
      deleteArmTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  async function beginRename(project, ev) {
    ev.stopPropagation();
    renamingId = project.id;
    renameDraft = project.name;
    await Promise.resolve();
    renameEl?.focus();
    renameEl?.select();
  }

  function commitRename() {
    if (!renamingId) return;
    const name = renameDraft.trim() || 'Untitled project';
    renameProject(renamingId, name);
    renamingId = null;
  }

  function cancelRename() {
    renamingId = null;
  }

  function onRenameKey(e) {
    if (e.key === 'Enter') { e.preventDefault(); commitRename(); }
    else if (e.key === 'Escape') { e.preventDefault(); cancelRename(); }
  }

  function sceneCount(projectId) {
    return scenesState.scenes.reduce((n, s) => n + (s.projectId === projectId ? 1 : 0), 0);
  }
</script>

<section class="wrap">
  <header class="head">
    <h1>Projects</h1>
    <p class="sub">Pick a project to load — Scenes and other tabs show its contents.</p>
  </header>

  <form class="new" onsubmit={onCreate}>
    <input
      type="text"
      placeholder="New project name"
      bind:value={newName}
      aria-label="New project name"
    />
    <button type="submit" disabled={!newName.trim()}>Create</button>
  </form>

  {#if deleteErr}
    <div class="delete-err">{deleteErr}</div>
  {/if}

  {#if scenesState.projects.length === 0}
    <div class="empty">No projects yet — create one above to get started.</div>
  {:else}
    <ul>
      {#each scenesState.projects as project (project.id)}
        <li class:selected={scenesState.selectedProjectId === project.id}>
          <button
            type="button"
            class="pick"
            onclick={() => onSelect(project.id)}
            aria-current={scenesState.selectedProjectId === project.id}
          >
            {#if renamingId === project.id}
              <input
                class="rename"
                bind:this={renameEl}
                bind:value={renameDraft}
                onkeydown={onRenameKey}
                onblur={commitRename}
                onclick={(e) => e.stopPropagation()}
              />
            {:else}
              <span class="name" ondblclick={(e) => beginRename(project, e)} role="presentation">
                {project.name}
              </span>
            {/if}
            <span class="meta">
              {sceneCount(project.id)} scene{sceneCount(project.id) === 1 ? '' : 's'}
              {#if scenesState.selectedProjectId === project.id}
                <span class="loaded">loaded</span>
              {/if}
            </span>
          </button>

          <div class="row-actions">
            <button
              class="icon-btn"
              title="Rename"
              onclick={(e) => beginRename(project, e)}
              aria-label="Rename project"
            >✎</button>
            <button
              class="icon-btn delete"
              class:armed={deleteArmedFor === project.id}
              title={deleteArmedFor === project.id ? 'Click again — deletes the project AND its scenes' : 'Delete project'}
              onclick={(e) => armDelete(project.id, e)}
              aria-label="Delete project"
            >{deleteArmedFor === project.id ? '?' : '×'}</button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .head h1 {
    margin: 0 0 4px;
    font-size: 20px;
    font-weight: 600;
    color: var(--text);
  }
  .sub {
    margin: 0;
    color: var(--muted);
    font-size: 13px;
  }

  .new {
    display: flex;
    gap: 8px;
  }
  .new input {
    flex: 1;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
    font-family: inherit;
  }
  .new input:focus { outline: none; border-color: var(--accent); }
  .new button {
    padding: 8px 14px;
    background: rgba(122,162,255,0.14);
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 6px;
    font-size: 13px;
    font-family: inherit;
    cursor: pointer;
  }
  .new button:hover:not(:disabled) { background: rgba(122,162,255,0.22); }
  .new button:disabled { opacity: 0.4; cursor: not-allowed; }

  .empty {
    padding: 24px;
    color: var(--muted);
    font-size: 13px;
    text-align: center;
    border: 1px dashed var(--border);
    border-radius: 8px;
  }
  .delete-err {
    padding: 8px 12px;
    color: var(--err);
    font-size: 12px;
    border: 1px solid var(--err);
    border-radius: 6px;
    background: rgba(255,128,128,0.06);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 6px 0 0;
    border-radius: 8px;
    border: 1px solid var(--border);
    background: var(--panel);
    color: var(--text);
    font-size: 13px;
    transition: background 0.1s, border-color 0.1s;
  }
  li:hover { background: rgba(255,255,255,0.03); }
  li.selected {
    border-color: var(--accent);
    background: rgba(122,162,255,0.10);
  }
  .pick {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 10px 12px;
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 36px;
  }
  .pick:focus-visible { outline: 1px solid var(--accent); outline-offset: -2px; border-radius: 6px; }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .meta {
    flex: 0 0 auto;
    display: flex;
    gap: 8px;
    align-items: center;
    color: var(--muted);
    font-size: 12px;
  }
  .loaded {
    color: var(--accent);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 10px;
    font-weight: 600;
  }
  .rename {
    flex: 1;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 4px 6px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
  }
  .row-actions {
    display: flex;
    gap: 2px;
    opacity: 0;
    transition: opacity 0.1s;
  }
  li:hover .row-actions,
  li.selected .row-actions,
  li:focus-within .row-actions { opacity: 1; }
  .icon-btn {
    background: transparent;
    color: var(--muted);
    border: none;
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 13px;
    cursor: pointer;
    line-height: 1;
  }
  .icon-btn:hover { color: var(--text); background: rgba(255,255,255,0.06); }
  .icon-btn.delete:hover { color: var(--err); }
  .icon-btn.delete.armed {
    color: var(--err);
    background: rgba(255,128,128,0.12);
  }
</style>
