<script>
  import SceneSidebar from '../components/SceneSidebar.svelte';
  import SpeakCell from '../components/SpeakCell.svelte';
  import { mixScene } from '../lib/api.js';
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
    sceneCharacters,
    getCharacter,
    resolveClipStyle,
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

  // Per-clip: is the cell currently showing its editing form? Cells with an
  // open editor are treated as "not yet rendered" for scene-level Play/Render
  // gating so the user can't accidentally play a stale blob while typing.
  let editing = $state({});

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

  function onVoiceChange(laneId, ev) {
    if (scene) setLaneVoice(scene.id, laneId, ev.currentTarget.value);
  }

  function onCellChange(clipId, snap) {
    if (scene) updateClip(scene.id, clipId, snap);
  }

  function onCellDelete(clipId) {
    if (scene) {
      deleteClip(scene.id, clipId);
      delete hasAudio[clipId];
      delete editing[clipId];
      delete cellPlayers[clipId];
    }
  }

  function onCellAudio(clipId, payload) {
    if (payload?.blobUrl) hasAudio[clipId] = true;
    else                  delete hasAudio[clipId];
  }

  function onCellEdit(clipId, isEditing) {
    if (isEditing) editing[clipId] = true;
    else           delete editing[clipId];
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

  // ---- Scene render -------------------------------------------------------
  // Renders every clip in scene order that doesn't already have a cached
  // blob. Sequential so the user sees each cell's own progress bar animate
  // in turn (and to avoid blasting the server with N parallel TTS jobs).
  // Clips with empty text are skipped — the user has to fill them in before
  // the scene can be fully rendered.
  let rendering = $state(false);
  let renderStopRequested = false;

  async function renderScene() {
    if (!scene) return;
    if (rendering) { stopRender(); return; }
    rendering = true;
    renderStopRequested = false;
    for (const clip of scene.clips) {
      if (renderStopRequested) break;
      const player = cellPlayers[clip.id];
      if (!player) continue;
      // Render if the cell has no cached blob yet, or if the user has the
      // editor open (potentially dirty text). Skip anything that would just
      // fail with "text is empty".
      const needsRender = !player.isReady() || editing[clip.id];
      if (!needsRender) continue;
      if (!player.hasText?.()) continue;
      try {
        await player.render({ autoplay: false });
      } catch {
        // Cell surfaces its own error; keep going so one failure doesn't
        // block the rest of the scene.
      }
    }
    rendering = false;
  }

  function stopRender() {
    renderStopRequested = true;
    rendering = false;
  }

  // ---- Scene download -----------------------------------------------------
  // Ask the server to concatenate every rendered clip in this scene into a
  // single FLAC (with the scene's own pauseMs of silence between clips) and
  // stream it back as a file download. Only meaningful when every clip is
  // rendered — the download button shares its visibility gate with Play.
  let downloading = $state(false);
  let downloadErr = $state('');

  async function downloadScene() {
    if (!scene || downloading) return;
    downloadErr = '';
    downloading = true;
    try {
      const clipIds = scene.clips.map((c) => c.widgetId).filter(Boolean);
      const { blob, filename } = await mixScene(scene.name, scene.pauseMs ?? 0, clipIds);
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = filename;
      document.body.appendChild(a);
      a.click();
      a.remove();
      // Give the browser a beat to start the download before we release the URL.
      setTimeout(() => URL.revokeObjectURL(url), 4000);
    } catch (e) {
      downloadErr = e?.message || 'download failed';
      setTimeout(() => { downloadErr = ''; }, 4000);
    } finally {
      downloading = false;
    }
  }

  const allRendered = $derived(
    scene && scene.clips.length > 0
      ? scene.clips.every((c) => hasAudio[c.id] && !editing[c.id])
      : false,
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

  // Characters the picker offers for this scene's lanes. Reactive on
  // characters state so newly-added ones appear immediately.
  const laneChoices = $derived(sceneCharacters(scene));

  // True when a lane's stored voice references a character that is no longer
  // in the project (deleted after the lane was created, or leftover free-text
  // from before the picker existed).
  function laneVoiceMissing(lane) {
    return !!lane.voice && !getCharacter(lane.voice);
  }

  // Voice payload sent to the server for a given clip's next render. The
  // effective `instruct` comes from the character's selected style (or the
  // default style if none picked yet). Empty object if the lane has no
  // valid character — the server then falls back to its startup default.
  function clipVoicePayload(clip) {
    if (!scene) return {};
    const lane = scene.lanes.find((l) => l.id === clip.laneId);
    const character = lane ? getCharacter(lane.voice) : null;
    if (!character) return {};
    const style = resolveClipStyle(character, clip);
    return {
      speaker: character.voice.speaker,
      language: character.voice.language,
      instruct: style.instruct,
      pitchSemitones: style.pitchSemitones,
      timeRatio: style.timeRatio,
    };
  }

  // Style choices offered by a clip's SpeakCell dropdown. Empty list if the
  // lane has no character; the SpeakCell then hides the picker.
  function clipStyleOptions(clip) {
    if (!scene) return [];
    const lane = scene.lanes.find((l) => l.id === clip.laneId);
    const character = lane ? getCharacter(lane.voice) : null;
    if (!character) return [];
    return character.voice.styles.map((s) => ({ id: s.id, name: s.name }));
  }
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
          {#if !rendering && (allRendered || playing)}
            <button
              class="play-scene"
              onclick={playScene}
              title={playing ? 'Stop scene' : 'Play whole scene'}
            >
              {#if playing}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><rect x="6" y="6" width="12" height="12" fill="currentColor"/></svg>
                <span>Stop</span>
              {:else}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="M8 5v14l11-7z"/></svg>
                <span>Play scene</span>
              {/if}
            </button>
            <button
              class="play-scene download"
              onclick={downloadScene}
              disabled={downloading || playing}
              title={downloading ? 'Building FLAC…' : 'Download scene as a single FLAC file'}
              aria-label="Download scene as FLAC"
            >
              {#if downloading}
                <span class="spinner" aria-hidden="true"></span>
                <span>Mixing…</span>
              {:else}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="M5 20h14v-2H5v2zm7-18-5.5 5.5 1.41 1.41L11 6.83V16h2V6.83l3.09 3.08 1.41-1.41z"/></svg>
                <span>Download</span>
              {/if}
            </button>
          {:else}
            <button
              class="play-scene"
              onclick={renderScene}
              disabled={scene.clips.length === 0}
              title={rendering ? 'Stop rendering' : (scene.clips.length === 0 ? 'Add a clip first' : 'Save and render every clip that is not yet rendered or has open edits')}
            >
              {#if rendering}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><rect x="6" y="6" width="12" height="12" fill="currentColor"/></svg>
                <span>Stop</span>
              {:else}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true"><path fill="currentColor" d="M12 6V3L8 7l4 4V8c3.31 0 6 2.69 6 6s-2.69 6-6 6-6-2.69-6-6H4c0 4.42 3.58 8 8 8s8-3.58 8-8-3.58-8-8-8z"/></svg>
                <span>Render scene</span>
              {/if}
            </button>
          {/if}
        </div>
      </div>

      {#if laneDeleteErr}
        <div class="lane-err">{laneDeleteErr}</div>
      {/if}
      {#if downloadErr}
        <div class="lane-err">{downloadErr}</div>
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
              <select
                class="voice-input"
                class:missing={laneVoiceMissing(lane)}
                value={lane.voice}
                onchange={(e) => onVoiceChange(lane.id, e)}
                aria-label="Character for this lane"
              >
                <option value="" disabled>— choose character —</option>
                {#if laneVoiceMissing(lane)}
                  <option value={lane.voice} disabled>(deleted character)</option>
                {/if}
                {#each laneChoices as ch (ch.id)}
                  <option value={ch.id}>{ch.name}</option>
                {/each}
              </select>
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
                initialStyleId={clip.styleId ?? null}
                initialWidgetId={clip.widgetId}
                startEditing={!clip.text && !clip.widgetId}
                voice={clipVoicePayload(clip)}
                styles={clipStyleOptions(clip)}
                onchange={(snap) => onCellChange(clip.id, snap)}
                ondelete={() => onCellDelete(clip.id)}
                onaudio={(payload) => onCellAudio(clip.id, payload)}
                onedit={(isEditing) => onCellEdit(clip.id, isEditing)}
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

  /* Small inline spinner for the "Mixing…" state — keeps the download button
     from resizing when it flips between icon and progress. */
  .play-scene .spinner {
    width: 12px;
    height: 12px;
    border: 2px solid rgba(122,162,255,0.35);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

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
    cursor: pointer;
  }
  .voice-input::placeholder { color: var(--muted); }
  .voice-input.missing { border-color: var(--err); color: var(--err); }
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
