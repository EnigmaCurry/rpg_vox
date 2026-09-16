<script>
  import { createSidebarDrawer } from '../lib/sidebarDrawer.svelte.js';

  // The Record page's left rail. Modelled on SceneSidebar / ScriptSidebar:
  // inline rename via ✎ or dblclick, two-click delete on ×, "+ new" in
  // the header. The parent (Record.svelte) owns the state (recordings
  // list, current selection, active-recording bucket) and passes callbacks
  // in so this component doesn't fetch anything itself.
  //
  // At 720p and below the sidebar collapses into a fixed hamburger button at
  // the top-left; tapping it slides the sidebar in as a drawer. Any
  // navigation choice (select row, new recording) auto-dismisses; the
  // backdrop and Escape also close it. See `sidebarDrawer.svelte.js`.
  //
  // Three kinds of sidebar entries, in this order:
  //   1. "Live buffer" — always at the top, always selectable. Renders
  //      the rolling not-recording transcript in the main pane.
  //   2. "● RECORDING: <name>" — pinned right below the live buffer
  //      when a recording is in flight. Auto-selected when the user
  //      hits "New Recording".
  //   3. Saved recordings, newest-first.
  //
  // The [+] in the header starts a fresh recording (parent handler
  // prompts for a name).
  let {
    recordings = [],
    activeRecording = null,
    selected = 'live',   // 'live' | 'active' | recording-id
    onSelect,
    onNewRecording,
    onRename,
    onDelete,
    // Called (with no args) when the user two-click-confirms the trash
    // button on the Live buffer row. Distinct callback from onDelete
    // because the buffer isn't a recording — it's cleared, not removed.
    onClearLive,
  } = $props();

  // Sentinel key for the Live buffer's armed-clear state so it can share
  // the same armed/timer machinery as the recordings' delete buttons
  // without ever colliding with a real recording id.
  const LIVE_CLEAR_KEY = '__live__';

  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  let renamingId = $state(null);
  let renameDraft = $state('');
  let renameEl = $state(null);

  function isSelected(key) {
    return selected === key;
  }

  // Two-click confirmation shared by the recording-delete buttons and
  // the Live buffer's clear button. `key` is the recording id for saved
  // rows, or `LIVE_CLEAR_KEY` for the Live buffer. The confirmed action
  // is chosen by the caller passing the right `onConfirm` — that keeps
  // this function free of any policy about *what* the confirmed click
  // means.
  async function armAction(key, onConfirm, ev) {
    ev.stopPropagation();
    if (deleteArmedFor === key) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      onConfirm?.();
      return;
    }
    deleteArmedFor = key;
    if (deleteArmTimer) clearTimeout(deleteArmTimer);
    deleteArmTimer = setTimeout(() => {
      deleteArmedFor = null;
      deleteArmTimer = 0;
    }, DELETE_CONFIRM_MS);
  }
  const armDelete = (id, ev) => armAction(id, () => onDelete?.(id), ev);
  const armClearLive = (ev) => armAction(LIVE_CLEAR_KEY, () => onClearLive?.(), ev);

  async function beginRename(rec, ev) {
    ev.stopPropagation();
    renamingId = rec.id;
    renameDraft = rec.name;
    await Promise.resolve();
    renameEl?.focus();
    renameEl?.select();
  }

  function commitRename() {
    if (!renamingId) return;
    const name = renameDraft.trim() || 'Untitled recording';
    const id = renamingId;
    renamingId = null;
    onRename?.(id, name);
  }

  function cancelRename() { renamingId = null; }

  function onRenameKey(e) {
    if (e.key === 'Enter') { e.preventDefault(); commitRename(); }
    else if (e.key === 'Escape') { e.preventDefault(); cancelRename(); }
  }

  function fmtDur(ms) {
    if (!Number.isFinite(ms) || ms <= 0) return '0:00';
    const total = Math.floor(ms / 1000);
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
  }

  const drawer = createSidebarDrawer();
  const selectAndClose = drawer.wrap((key) => onSelect?.(key));
  const newAndClose = drawer.wrap(() => onNewRecording?.());
</script>

{#if drawer.isMobile && !drawer.open}
  <button
    type="button"
    class="drawer-hamburger"
    onclick={() => drawer.setOpen(true)}
    aria-label="Open recordings menu"
    aria-expanded="false"
  >☰</button>
{/if}

<aside class="sidebar" class:drawer={drawer.isMobile} class:open={drawer.open}>
  <div class="head">
    <span class="title">Recordings</span>
    <button
      class="new-btn"
      onclick={newAndClose}
      disabled={activeRecording != null}
      title={activeRecording != null ? 'Stop the current recording first' : 'New recording'}
    >+</button>
  </div>

  <ul>
    <li class:selected={isSelected('live')}>
      <button
        type="button"
        class="pick"
        onclick={() => selectAndClose('live')}
        aria-current={isSelected('live')}
      >
        <span class="icon" aria-hidden="true">≡</span>
        <span class="name">Live buffer</span>
      </button>
      <div class="row-actions">
        <button
          class="icon-btn delete"
          class:armed={deleteArmedFor === LIVE_CLEAR_KEY}
          title={deleteArmedFor === LIVE_CLEAR_KEY ? 'Click again to confirm' : 'Clear live buffer'}
          onclick={(e) => armClearLive(e)}
          aria-label="Clear live buffer"
        >{deleteArmedFor === LIVE_CLEAR_KEY ? '?' : '×'}</button>
      </div>
    </li>

    {#if activeRecording}
      <li class:selected={isSelected('active')} class="active-item">
        <button
          type="button"
          class="pick"
          onclick={() => selectAndClose('active')}
          aria-current={isSelected('active')}
        >
          <span class="rec-dot" aria-hidden="true"></span>
          {#if renamingId === activeRecording.id}
            <input
              class="rename"
              bind:this={renameEl}
              bind:value={renameDraft}
              onkeydown={onRenameKey}
              onblur={commitRename}
              onclick={(e) => e.stopPropagation()}
            />
          {:else}
            <span
              class="name"
              title={activeRecording.name}
              ondblclick={(e) => beginRename(activeRecording, e)}
              role="presentation"
            >{activeRecording.name}</span>
          {/if}
          <span class="rec-time">{fmtDur(activeRecording.duration_ms ?? activeRecording.durationMs ?? 0)}</span>
        </button>
        <div class="row-actions">
          <button
            class="icon-btn"
            title="Rename"
            onclick={(e) => beginRename(activeRecording, e)}
            aria-label="Rename active recording"
          >✎</button>
        </div>
      </li>
    {/if}

    {#if recordings.length > 0}
      <li class="section-header"><span>Saved</span></li>
      {#each recordings as rec (rec.id)}
        <li class:selected={isSelected(rec.id)}>
          <button
            type="button"
            class="pick"
            onclick={() => selectAndClose(rec.id)}
            aria-current={isSelected(rec.id)}
          >
            {#if renamingId === rec.id}
              <input
                class="rename"
                bind:this={renameEl}
                bind:value={renameDraft}
                onkeydown={onRenameKey}
                onblur={commitRename}
                onclick={(e) => e.stopPropagation()}
              />
            {:else}
              <span
                class="name"
                title={rec.name}
                ondblclick={(e) => beginRename(rec, e)}
                role="presentation"
              >{rec.name}</span>
            {/if}
          </button>
          <div class="row-actions">
            <button
              class="icon-btn"
              title="Rename"
              onclick={(e) => beginRename(rec, e)}
              aria-label="Rename recording"
            >✎</button>
            <button
              class="icon-btn delete"
              class:armed={deleteArmedFor === rec.id}
              title={deleteArmedFor === rec.id ? 'Click again to confirm' : 'Delete recording'}
              onclick={(e) => armDelete(rec.id, e)}
              aria-label="Delete recording"
            >{deleteArmedFor === rec.id ? '?' : '×'}</button>
          </div>
        </li>
      {/each}
    {/if}
  </ul>
</aside>

{#if drawer.isMobile && drawer.open}
  <button
    type="button"
    class="drawer-backdrop"
    onclick={() => drawer.close()}
    aria-label="Close recordings menu"
  ></button>
{/if}

<style>
  .sidebar {
    width: 240px;
    flex: 0 0 240px;
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
  .title { font-size: 13px; font-weight: 600; color: var(--text); }
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
  li.section-header {
    padding: 10px 8px 4px;
    color: var(--muted);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-weight: 600;
    border-top: 1px solid var(--border);
    margin-top: 6px;
    background: transparent;
    cursor: default;
  }
  li.section-header:hover { background: transparent; }
  li.active-item {
    background: rgba(255,80,80,0.06);
  }
  li.active-item.selected {
    background: rgba(255,80,80,0.14);
    box-shadow: inset 2px 0 0 var(--err);
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
    gap: 8px;
    min-height: 28px;
  }
  .pick:focus-visible { outline: 1px solid var(--accent); outline-offset: -2px; border-radius: 4px; }
  .icon {
    color: var(--muted);
    font-size: 14px;
    line-height: 1;
    flex-shrink: 0;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--err);
    box-shadow: 0 0 4px rgba(255,80,80,0.7);
    animation: rec-pulse 1.2s ease-in-out infinite;
    flex-shrink: 0;
  }
  .rec-time {
    color: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }
  @keyframes rec-pulse {
    0%, 100% { opacity: 0.55; transform: scale(0.9); }
    50%      { opacity: 1;    transform: scale(1.15); }
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
