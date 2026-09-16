<script>
  import {
    scenesState,
    createScene,
    selectScene,
    renameScene,
    deleteScene,
    currentProjectScenes,
    currentProject,
    currentProjectCharacters,
  } from '../lib/scenes.svelte.js';
  import { createSidebarDrawer } from '../lib/sidebarDrawer.svelte.js';

  // Two-click confirm on delete, per-scene id. Rename is inline via a
  // dblclick → contenteditable-ish pattern using a text input.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  let deleteErr = $state('');
  const DELETE_CONFIRM_MS = 2500;

  let renamingId = $state(null);
  let renameDraft = $state('');
  let renameEl = $state(null);

  const scenes = $derived(currentProjectScenes());
  const project = $derived(currentProject());
  const hasProject = $derived(project != null);
  const characters = $derived(currentProjectCharacters());
  const hasCharacter = $derived(characters.length > 0);
  const canCreateScene = $derived(hasProject && hasCharacter);

  function onNew() {
    if (!canCreateScene) return;
    createScene(`Scene ${scenes.length + 1}`);
  }

  function onSelect(id) {
    if (renamingId) return;
    selectScene(id);
  }

  async function armDelete(id, ev) {
    ev.stopPropagation();
    if (deleteArmedFor === id) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      const { failed } = (await deleteScene(id)) ?? { failed: 0 };
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

  async function beginRename(scene, ev) {
    ev.stopPropagation();
    renamingId = scene.id;
    renameDraft = scene.name;
    await Promise.resolve();
    renameEl?.focus();
    renameEl?.select();
  }

  function commitRename() {
    if (!renamingId) return;
    const name = renameDraft.trim() || 'Untitled scene';
    renameScene(renamingId, name);
    renamingId = null;
  }

  function cancelRename() {
    renamingId = null;
  }

  function onRenameKey(e) {
    if (e.key === 'Enter') { e.preventDefault(); commitRename(); }
    else if (e.key === 'Escape') { e.preventDefault(); cancelRename(); }
  }

  // At 720p and below the sidebar collapses to a hamburger
  // drawer; selecting a scene or hitting `+` dismisses it.
  const drawer = createSidebarDrawer();
  const selectAndClose = drawer.wrap(onSelect);
  const newAndClose = drawer.wrap(onNew);
</script>

{#if drawer.isMobile && !drawer.open}
  <button
    type="button"
    class="drawer-hamburger"
    onclick={() => drawer.setOpen(true)}
    aria-label="Open scenes menu"
    aria-expanded="false"
  >☰</button>
{/if}

<aside class="sidebar" class:drawer={drawer.isMobile} class:open={drawer.open}>
  <div class="head">
    <span class="title" title={project?.name ?? ''}>
      {project ? project.name : 'No project'}
    </span>
    <button
      class="new-btn"
      onclick={newAndClose}
      disabled={!canCreateScene}
      title={!hasProject ? 'Load a project first' : (!hasCharacter ? 'Add a character first' : 'New scene')}
    >+</button>
  </div>

  {#if deleteErr}
    <div class="delete-err">{deleteErr}</div>
  {/if}

  {#if !hasProject}
    <div class="empty">Load a project from the <b>Projects</b> tab to add scenes.</div>
  {:else if !hasCharacter}
    <div class="empty">
      Add at least one character in the <b><a href="#/characters">Characters</a></b> tab before creating scenes.
    </div>
  {:else if scenes.length === 0}
    <div class="empty">No scenes yet.<br/>Click <b>+</b> to create one.</div>
  {:else}
    <ul>
      {#each scenes as scene (scene.id)}
        <li class:selected={scenesState.selectedSceneId === scene.id}>
          <button
            type="button"
            class="pick"
            onclick={() => selectAndClose(scene.id)}
            aria-current={scenesState.selectedSceneId === scene.id}
          >
            {#if renamingId === scene.id}
              <input
                class="rename"
                bind:this={renameEl}
                bind:value={renameDraft}
                onkeydown={onRenameKey}
                onblur={commitRename}
                onclick={(e) => e.stopPropagation()}
              />
            {:else}
              <span class="name" ondblclick={(e) => beginRename(scene, e)} role="presentation">{scene.name}</span>
            {/if}
          </button>

          <div class="row-actions">
            <button
              class="icon-btn"
              title="Rename"
              onclick={(e) => beginRename(scene, e)}
              aria-label="Rename scene"
            >✎</button>
            <button
              class="icon-btn delete"
              class:armed={deleteArmedFor === scene.id}
              title={deleteArmedFor === scene.id ? 'Click again to confirm' : 'Delete scene'}
              onclick={(e) => armDelete(scene.id, e)}
              aria-label="Delete scene"
            >{deleteArmedFor === scene.id ? '?' : '×'}</button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</aside>

{#if drawer.isMobile && drawer.open}
  <button
    type="button"
    class="drawer-backdrop"
    onclick={() => drawer.close()}
    aria-label="Close scenes menu"
  ></button>
{/if}

<style>
  .sidebar {
    width: 220px;
    flex: 0 0 220px;
    border-right: 1px solid var(--border);
    background: var(--panel);
    padding: 12px 8px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 6px 6px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .new-btn {
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    width: 22px;
    height: 22px;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
  }
  .new-btn:hover:not(:disabled) { color: var(--accent); border-color: var(--accent); }
  .new-btn:disabled { opacity: 0.4; cursor: not-allowed; }

  .empty {
    padding: 16px 8px;
    font-size: 12px;
    color: var(--muted);
    text-align: center;
    line-height: 1.5;
  }
  .empty a { color: var(--accent); }
  .delete-err {
    font-size: 11px;
    color: var(--err);
    padding: 4px 8px;
    line-height: 1.4;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
    min-height: 0;
  }
  li {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 4px 0 0;
    border-radius: 6px;
    color: var(--text);
    font-size: 13px;
    background: transparent;
    transition: background 0.1s;
  }
  li:hover { background: rgba(255,255,255,0.04); }
  li.selected {
    background: rgba(122,162,255,0.14);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .pick {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 6px 8px;
    cursor: pointer;
    display: flex;
    align-items: center;
    min-height: 28px;
  }
  .pick:focus-visible { outline: 1px solid var(--accent); outline-offset: -2px; border-radius: 4px; }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rename {
    flex: 1;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 2px 6px;
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
    padding: 2px 6px;
    border-radius: 4px;
    font-size: 12px;
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
