<script>
  import SceneSidebar from '../components/SceneSidebar.svelte';
  import SpeakCell from '../components/SpeakCell.svelte';
  import {
    scenesState,
    addLane,
    insertClip,
    updateClip,
    deleteClip,
    deleteLane,
    setLaneVoice,
    laneHasClips,
    setScenePause,
    currentProjectCharacters,
    PAUSE_OPTIONS,
  } from '../lib/scenes.svelte.js';

  function pauseLabel(ms) {
    if (ms === 0) return 'no pause';
    if (ms < 1000) return `${ms} ms`;
    return `${(ms / 1000).toFixed(ms % 1000 === 0 ? 0 : 1)} s`;
  }

  function onPauseChange(ev) {
    if (scene) setScenePause(scene.id, Number(ev.currentTarget.value));
  }

  const scene = $derived(
    scenesState.selectedSceneId
      ? scenesState.scenes.find((s) => s.id === scenesState.selectedSceneId) ?? null
      : null,
  );

  const hasCharacter = $derived(currentProjectCharacters().length > 0);

  // Per-clip transient state: whether each cell has a playable blob (used
  // for enabling the Play scene button + skipping unrendered clips during
  // playback). SpeakCell emits `onaudio` after render or hydration.
  let hasAudio = $state({});

  // Handles the SpeakCell binds on mount so scene playback can drive each
  // cell's own <audio> + progress animation. Value is null while a cell is
  // between mount and its onMount, or after unmount.
  let cellPlayers = {};   // clipId → { playToEnd, stop, isReady }
  let currentPlayer = null;

  // Two-tap confirm for lane deletes, keyed by laneId.
  let laneDeleteArmed = $state(null);
  let laneDeleteTimer = 0;
  let laneDeleteErr = $state('');
  const DELETE_CONFIRM_MS = 2500;

  function onAddLane() {
    if (scene) addLane(scene.id);
  }

  function onInsertClip(laneId, position) {
    if (scene) insertClip(scene.id, laneId, position);
  }

  function onVoiceInput(laneId, ev) {
    if (scene) setLaneVoice(scene.id, laneId, ev.currentTarget.value);
  }

  function onCellChange(clipId, snap) {
    if (scene) updateClip(scene.id, clipId, snap);
  }

  function onCellDelete(clipId) {
    if (scene) {
      deleteClip(scene.id, clipId);
      delete hasAudio[clipId];
      delete cellPlayers[clipId];
    }
  }

  function onCellAudio(clipId, payload) {
    if (payload?.blobUrl) hasAudio[clipId] = true;
    else                  delete hasAudio[clipId];
  }

  function onCellBind(clipId, handle) {
    if (handle) cellPlayers[clipId] = handle;
    else        delete cellPlayers[clipId];
  }

  function onDeleteLane(laneId) {
    if (!scene) return;
    if (laneHasClips(scene, laneId)) {
      laneDeleteErr = 'clear clips in this lane first';
      setTimeout(() => { laneDeleteErr = ''; }, 2000);
      return;
    }
    if (laneDeleteArmed === laneId) {
      if (laneDeleteTimer) clearTimeout(laneDeleteTimer);
      laneDeleteArmed = null;
      deleteLane(scene.id, laneId);
      return;
    }
    laneDeleteArmed = laneId;
    if (laneDeleteTimer) clearTimeout(laneDeleteTimer);
    laneDeleteTimer = setTimeout(() => {
      laneDeleteArmed = null;
      laneDeleteTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  // ---- Scene playback -----------------------------------------------------
  // Walks scene.clips in order (the timeline, since lanes are y-exclusive)
  // and drives each cell's own play() so the cell's progress bar animates
  // just like a solo play. Skips clips that haven't rendered yet.
  let playing = $state(false);
  let currentClipId = $state(null);
  let stopRequested = false;

  async function playScene() {
    if (!scene) return;
    if (playing) { stopScene(); return; }
    playing = true;
    stopRequested = false;
    const gap = Math.max(0, scene.pauseMs ?? 0);
    let first = true;
    for (const clip of scene.clips) {
      if (stopRequested) break;
      const player = cellPlayers[clip.id];
      if (!player?.isReady()) continue;
      if (!first && gap > 0) {
        await new Promise((resolve) => setTimeout(resolve, gap));
        if (stopRequested) break;
      }
      first = false;
      currentClipId = clip.id;
      currentPlayer = player;
      try {
        await player.playToEnd();
      } catch {
        // Skip failing clip and continue with the next.
      }
    }
    currentClipId = null;
    currentPlayer = null;
    playing = false;
  }

  function stopScene() {
    stopRequested = true;
    try { currentPlayer?.stop(); } catch {}
    currentPlayer = null;
    playing = false;
    currentClipId = null;
  }

  const anyPlayable = $derived(
    scene ? scene.clips.some((c) => hasAudio[c.id]) : false,
  );

  // Grid template: each lane is a fixed-width track; row heights auto-fit
  // whichever cell is populated in that row.
  const gridCols = $derived(
    scene ? scene.lanes.map(() => '320px').join(' ') : '',
  );

  // Grid rows:
  //   1              — sticky lane headers
  //   2 + 2*pos      — inserter at position `pos` (pos ∈ [0, clips.length])
  //   3 + 2*i        — clip row for scene.clips[i]
  //
  // A scene with N clips uses 1 header + (N+1) inserter + N clip rows.
  function inserterRow(pos) { return 2 + pos * 2; }
  function clipRow(i)       { return 3 + i * 2; }
  function laneColumn(laneId) {
    if (!scene) return 0;
    const idx = scene.lanes.findIndex((l) => l.id === laneId);
    return idx < 0 ? 0 : idx + 1;
  }

  // Inserter positions: one more than clip count (before each clip + after
  // the last). Derived so it's reactive with scene.clips length.
  const inserterPositions = $derived(
    scene ? Array.from({ length: scene.clips.length + 1 }, (_, i) => i) : [],
  );
</script>

<div class="page">
  <SceneSidebar />

  <div class="main">
    {#if !scenesState.selectedProjectId}
      <div class="hint">
        Load a project from the <a href="#/projects">Projects</a> tab to view its scenes.
      </div>
    {:else if !hasCharacter}
      <div class="hint">
        Add at least one character in the <a href="#/characters">Characters</a> tab before working on scenes.
      </div>
    {:else if !scene}
      <div class="hint">
        {#if scenesState.scenes.filter((s) => s.projectId === scenesState.selectedProjectId).length === 0}
          Create a scene from the sidebar to get started.
        {:else}
          Select a scene from the sidebar.
        {/if}
      </div>
    {:else}
      <div class="scene-head">
        <h2>{scene.name}</h2>
        <div class="scene-actions">
          <label class="pause-picker" title="Default silence inserted between clips during playback">
            <span>Pause</span>
            <select value={scene.pauseMs} onchange={onPauseChange}>
              {#each PAUSE_OPTIONS as ms}
                <option value={ms}>{pauseLabel(ms)}</option>
              {/each}
            </select>
          </label>
          <button
            class="play-scene"
            onclick={playScene}
            disabled={!anyPlayable && !playing}
            title={playing ? 'Stop scene' : (anyPlayable ? 'Play whole scene' : 'Render at least one clip first')}
          >
            {#if playing}
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><rect x="6" y="6" width="12" height="12" fill="currentColor"/></svg>
              <span>Stop</span>
            {:else}
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="M8 5v14l11-7z"/></svg>
              <span>Play scene</span>
            {/if}
          </button>
        </div>
      </div>

      {#if laneDeleteErr}
        <div class="lane-err">{laneDeleteErr}</div>
      {/if}

      <div class="scroll">
        <div class="scene-grid" style:grid-template-columns={gridCols}>
          <!-- Row 1: sticky lane headers -->
          {#each scene.lanes as lane, j (lane.id)}
            <div
              class="lane-head"
              style:grid-column={j + 1}
              style:grid-row="1"
            >
              <input
                class="voice-input"
                type="text"
                value={lane.voice}
                oninput={(e) => onVoiceInput(lane.id, e)}
                placeholder="Voice / character"
              />
              <button
                class="del"
                class:armed={laneDeleteArmed === lane.id}
                class:blocked={laneHasClips(scene, lane.id)}
                onclick={() => onDeleteLane(lane.id)}
                title={laneHasClips(scene, lane.id) ? 'Delete clips first' : (laneDeleteArmed === lane.id ? 'Click again to confirm' : 'Delete lane')}
                aria-label="Delete lane"
              >×</button>
            </div>
          {/each}

          <!-- Clip cells: one per scene.clips entry, positioned by (laneId, index) -->
          {#each scene.clips as clip, i (clip.id)}
            <div
              class="clip-cell"
              class:active={currentClipId === clip.id}
              style:grid-column={laneColumn(clip.laneId)}
              style:grid-row={clipRow(i)}
            >
              <SpeakCell
                initialText={clip.text}
                initialInstruct={clip.instruct}
                initialWidgetId={clip.widgetId}
                startEditing={!clip.text && !clip.widgetId}
                onchange={(snap) => onCellChange(clip.id, snap)}
                ondelete={() => onCellDelete(clip.id)}
                onaudio={(payload) => onCellAudio(clip.id, payload)}
                bindPlayer={(handle) => onCellBind(clip.id, handle)}
              />
            </div>
          {/each}

          <!-- Inserter rows: one above every clip and one after the last.
               Each cell offers a "+" that appends into the given lane at
               that timeline position. Rows are thin and buttons are quiet
               until the row is hovered, so the grid stays readable. -->
          {#each inserterPositions as pos (pos)}
            {#each scene.lanes as lane, j (lane.id + ':ins:' + pos)}
              <div
                class="inserter"
                style:grid-column={j + 1}
                style:grid-row={inserterRow(pos)}
              >
                <button
                  class="ins-btn"
                  onclick={() => onInsertClip(lane.id, pos)}
                  title={pos === 0 ? 'Insert clip at start' : (pos === scene.clips.length ? 'Add clip to end' : `Insert clip at position ${pos + 1}`)}
                  aria-label="Insert clip"
                >+</button>
              </div>
            {/each}
          {/each}
        </div>

        <button class="add-lane" onclick={onAddLane} title="Add lane">
          <span class="plus">+</span>
          <span>lane</span>
        </button>
      </div>
    {/if}
  </div>

</div>

<style>
  .page {
    display: flex;
    gap: 0;
    height: calc(100vh - 46px);
    min-height: 0;
    align-items: stretch;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    padding: 16px 20px;
    gap: 14px;
    overflow: hidden;
  }
  .hint {
    margin: auto;
    color: var(--muted);
    font-size: 14px;
  }

  .scene-head {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .scene-head h2 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    color: var(--text);
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .scene-actions { display: flex; gap: 10px; align-items: center; }
  .pause-picker {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
  }
  .pause-picker select {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 6px;
    font-size: 12px;
    font-family: inherit;
    cursor: pointer;
  }
  .pause-picker select:focus { outline: none; border-color: var(--accent); }
  .play-scene {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    background: rgba(122,162,255,0.14);
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 6px;
    font-size: 13px;
    font-family: inherit;
    cursor: pointer;
  }
  .play-scene:hover:not(:disabled) { background: rgba(122,162,255,0.22); }
  .play-scene:disabled { opacity: 0.4; cursor: not-allowed; }

  .lane-err {
    font-size: 12px;
    color: var(--err);
  }

  .scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding-bottom: 24px;
    display: flex;
    align-items: flex-start;
    gap: 14px;
  }

  .scene-grid {
    display: grid;
    gap: 10px 14px;
    align-items: start;
  }

  .lane-head {
    position: sticky;
    top: 0;
    z-index: 5;
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 6px;
    align-items: center;
    padding: 8px 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .voice-input {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
  }
  .voice-input::placeholder { color: var(--muted); }
  .voice-input:focus { outline: none; border-color: var(--accent); }
  .del {
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 6px;
    width: 28px;
    height: 28px;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
  }
  .del:hover:not(.blocked) { color: var(--err); border-color: var(--err); }
  .del.armed { color: var(--err); border-color: var(--err); background: rgba(255,128,128,0.10); }
  .del.blocked { opacity: 0.5; cursor: not-allowed; }

  .clip-cell {
    display: flex;
    justify-content: center;
    padding: 2px;
    border-radius: 10px;
    transition: box-shadow 0.15s;
  }
  .clip-cell.active { box-shadow: 0 0 0 2px var(--accent); }

  /* Thin gap row — a subtle rule with a "+" that becomes visible on hover.
     Hovering anywhere in the row highlights every column's "+" so the user
     sees that all lanes are insertable at this timeline position. */
  .inserter {
    height: 18px;
    display: flex;
    align-items: center;
    justify-content: center;
    position: relative;
  }
  .inserter::before {
    content: '';
    position: absolute;
    left: 12%;
    right: 12%;
    top: 50%;
    height: 1px;
    background: var(--border);
    opacity: 0.35;
    transition: opacity 0.1s, background 0.1s;
  }
  .ins-btn {
    position: relative;
    z-index: 1;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    padding: 0;
    background: var(--panel);
    color: var(--muted);
    border: 1px dashed var(--border);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0.35;
    transition: opacity 0.1s, color 0.1s, border-color 0.1s, background 0.1s;
  }
  .scene-grid:hover .ins-btn { opacity: 0.7; }
  .ins-btn:hover {
    opacity: 1;
    color: var(--accent);
    border-color: var(--accent);
    border-style: solid;
    background: rgba(122,162,255,0.08);
  }
  .inserter:hover::before { background: var(--accent); opacity: 0.5; }

  .add-lane {
    flex: 0 0 120px;
    align-self: stretch;
    background: transparent;
    color: var(--muted);
    border: 1px dashed var(--border);
    border-radius: 10px;
    padding: 20px 12px;
    font-size: 13px;
    cursor: pointer;
    font-family: inherit;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    min-height: 120px;
  }
  .add-lane:hover { color: var(--accent); border-color: var(--accent); background: rgba(122,162,255,0.04); }
  .add-lane .plus { font-size: 24px; font-weight: 300; line-height: 1; }
</style>
