<script>
  // Sidebar list of stored scripts. Selecting a row navigates the hash to
  // `/script/:id`, which App.svelte routes back into Script.svelte with a
  // fresh mount and a load-by-id effect. New/rename/delete talk to
  // /scripts/* via the api and refresh the store.

  import { navigate } from '../lib/router.js';
  import { scripts } from '../lib/stores.js';
  import { createScript, deleteScript, renameScript } from '../lib/api.js';
  import { createSidebarDrawer } from '../lib/sidebarDrawer.svelte.js';

  let {
    currentScriptId = null,
    onchange = null,
  } = $props();

  let renamingId = $state(null);
  let renameDraft = $state('');
  let renameEl = $state(null);
  let creating = $state(false);
  let error = $state('');

  // Two-click confirm on delete so a slip doesn't nuke a script full of
  // recordings. Timer resets the armed state if the user hesitates.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  function selectScript(id) {
    if (renamingId) return;
    onchange?.(id);
    navigate(`/script/${id}`);
  }

  async function onNew() {
    if (creating) return;
    creating = true;
    error = '';
    try {
      const created = await createScript();
      scripts.update((list) => [created, ...(list || [])]);
      // Auto-navigate straight into the new script so the user can start
      // typing without an extra click.
      selectScript(created.id);
    } catch (e) {
      error = `create: ${e.message || e}`;
    } finally {
      creating = false;
    }
  }

  function startRename(s, ev) {
    ev.stopPropagation();
    renamingId = s.id;
    renameDraft = s.name;
    setTimeout(() => renameEl?.focus(), 0);
  }
  function cancelRename() {
    renamingId = null;
    renameDraft = '';
  }
  async function commitRename(s) {
    const name = renameDraft.trim();
    if (!name || name === s.name) {
      cancelRename();
      return;
    }
    try {
      await renameScript(s.id, name);
      scripts.update((list) =>
        (list || []).map((x) => (x.id === s.id ? { ...x, name } : x))
      );
    } catch (e) {
      error = `rename: ${e.message || e}`;
    } finally {
      cancelRename();
    }
  }
  function onRenameKey(s, ev) {
    if (ev.key === 'Enter') { ev.preventDefault(); commitRename(s); }
    else if (ev.key === 'Escape') { ev.preventDefault(); cancelRename(); }
  }

  async function armDelete(s, ev) {
    ev.stopPropagation();
    if (deleteArmedFor === s.id) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      try {
        await deleteScript(s.id);
        scripts.update((list) => (list || []).filter((x) => x.id !== s.id));
        // If the deleted script was the current one, route the user to
        // the next-most-recent script (or nowhere if empty).
        if (s.id === currentScriptId) {
          const remaining = ($scripts || []).filter((x) => x.id !== s.id);
          if (remaining.length > 0) selectScript(remaining[0].id);
          else navigate('/script');
        }
      } catch (e) {
        error = `delete: ${e.message || e}`;
      }
      return;
    }
    deleteArmedFor = s.id;
    if (deleteArmTimer) clearTimeout(deleteArmTimer);
    deleteArmTimer = setTimeout(() => {
      deleteArmedFor = null;
      deleteArmTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  // At 720p and below collapse into a hamburger drawer. Selecting a script
  // (which triggers a hash-navigate) or hitting `+` dismisses it.
  const drawer = createSidebarDrawer();
  const selectAndClose = drawer.wrap(selectScript);
  const newAndClose = drawer.wrap(onNew);
</script>

{#if drawer.isMobile && !drawer.open}
  <button
    type="button"
    class="drawer-hamburger"
    onclick={() => drawer.setOpen(true)}
    aria-label="Open scripts menu"
    aria-expanded="false"
  >☰</button>
{/if}

<aside class="sidebar" class:drawer={drawer.isMobile} class:open={drawer.open}>
  <header>
    <span class="title">Scripts</span>
    <div class="hdr-actions">
      <button class="new" onclick={newAndClose} disabled={creating} title="Create a new script">
        +
      </button>
    </div>
  </header>

  {#if error}<div class="err">{error}</div>{/if}

  <ul>
    {#each $scripts as s (s.id)}
      <li class:active={s.id === currentScriptId}>
        {#if renamingId === s.id}
          <input
            class="rename"
            bind:this={renameEl}
            bind:value={renameDraft}
            onblur={() => commitRename(s)}
            onkeydown={(e) => onRenameKey(s, e)}
          />
        {:else}
          <button
            class="row-select"
            onclick={() => selectAndClose(s.id)}
            ondblclick={(e) => startRename(s, e)}
            title="Open this script (double-click to rename)"
          >
            <span class="name">{s.name}</span>
          </button>
        {/if}
        <div class="row-actions">
          <button
            class="row-btn"
            title="Rename"
            aria-label="Rename"
            onclick={(e) => startRename(s, e)}
          >
            <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
              <path fill="currentColor" d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04c.39-.39.39-1.02 0-1.41l-2.34-2.34a.996.996 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z"/>
            </svg>
          </button>
          <button
            class="row-btn danger"
            class:armed={deleteArmedFor === s.id}
            title={deleteArmedFor === s.id ? 'Click again to delete' : 'Delete'}
            aria-label="Delete"
            onclick={(e) => armDelete(s, e)}
          >
            <svg viewBox="0 0 24 24" width="12" height="12" aria-hidden="true">
              <path fill="currentColor" d="M9 3v1H4v2h1v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V6h1V4h-5V3H9zm2 5h2v9h-2V8zm-4 0h2v9H7V8zm8 0h2v9h-2V8z"/>
            </svg>
          </button>
        </div>
      </li>
    {/each}
    {#if ($scripts ?? []).length === 0}
      <li class="empty">No scripts yet. Click + to start.</li>
    {/if}
  </ul>
</aside>

{#if drawer.isMobile && drawer.open}
  <button
    type="button"
    class="drawer-backdrop"
    onclick={() => drawer.close()}
    aria-label="Close scripts menu"
  ></button>
{/if}

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--panel);
    border-right: 1px solid var(--border);
    overflow: hidden;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
  }
  .hdr-actions {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  button.new {
    width: 24px;
    height: 24px;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
    font-size: 14px;
  }
  button.new:hover:not(:disabled) {
    color: var(--accent);
    border-color: var(--accent);
  }
  .err {
    color: var(--err);
    font-size: 11px;
    padding: 6px 12px;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 6px 6px 12px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px 0 4px;
    margin: 2px 0;
    border-radius: 6px;
    color: var(--text);
    font-size: 13px;
    line-height: 1.3;
  }
  li:hover { background: rgba(255, 255, 255, 0.04); }
  li.active {
    background: rgba(122, 162, 255, 0.14);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  li.empty {
    color: var(--muted);
    font-style: italic;
    justify-content: center;
    cursor: default;
    padding: 8px 10px;
  }
  li.empty:hover { background: transparent; }

  /* Primary click target — proper button so keyboard nav + focus states
     work without the a11y warnings a plain-li-with-role would trigger. */
  .row-select {
    flex: 1;
    min-width: 0;
    padding: 8px 6px;
    background: transparent;
    color: inherit;
    border: none;
    text-align: left;
    cursor: pointer;
    font: inherit;
  }
  .row-select:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
    border-radius: 4px;
  }
  .name {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rename {
    flex: 1;
    background: rgba(0, 0, 0, 0.3);
    color: var(--text);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 13px;
    font-family: inherit;
  }
  .rename:focus { outline: none; }

  .row-actions {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    opacity: 0;
    transition: opacity 0.1s ease-in-out;
  }
  li:hover .row-actions,
  li:focus-within .row-actions,
  li.active .row-actions {
    opacity: 1;
  }
  .row-btn {
    padding: 3px;
    width: 22px;
    height: 22px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    color: var(--muted);
    border: 1px solid transparent;
    border-radius: 4px;
    cursor: pointer;
  }
  .row-btn:hover { color: var(--text); background: rgba(255, 255, 255, 0.06); }
  .row-btn.danger:hover, .row-btn.danger.armed {
    color: var(--err);
    background: rgba(255, 128, 128, 0.10);
    border-color: rgba(255, 128, 128, 0.35);
  }
</style>
