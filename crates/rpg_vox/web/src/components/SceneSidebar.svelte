<script>
  import { scenesState, createScene, selectScene, renameScene, deleteScene } from '../lib/scenes.svelte.js';

  // Two-click confirm on delete, per-scene id. Rename is inline via a
  // dblclick → contenteditable-ish pattern using a text input.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  let renamingId = $state(null);
  let renameDraft = $state('');
  let renameEl = $state(null);

  function onNew() {
    createScene(`Scene ${scenesState.scenes.length + 1}`);
  }

  function onSelect(id) {
    if (renamingId) return;
    selectScene(id);
  }

  function armDelete(id, ev) {
    ev.stopPropagation();
    if (deleteArmedFor === id) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      deleteScene(id);
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
</script>

<aside class="sidebar">
  <div class="head">
    <span class="title">Scenes</span>
    <button class="new-btn" onclick={onNew} title="New scene">+</button>
  </div>

  {#if scenesState.scenes.length === 0}
    <div class="empty">No scenes yet.<br/>Click <b>+</b> to create one.</div>
  {:else}
    <ul>
      {#each scenesState.scenes as scene (scene.id)}
        <li class:selected={scenesState.selectedSceneId === scene.id}>
          <button
            type="button"
            class="pick"
            onclick={() => onSelect(scene.id)}
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
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
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
  .new-btn:hover { color: var(--accent); border-color: var(--accent); }

  .empty {
    padding: 16px 8px;
    font-size: 12px;
    color: var(--muted);
    text-align: center;
    line-height: 1.5;
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
