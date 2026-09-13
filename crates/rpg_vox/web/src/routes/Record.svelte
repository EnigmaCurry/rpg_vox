<script>
  import { onMount, onDestroy, tick } from 'svelte';
  import * as api from '../lib/api.js';
  import RecordingsSidebar from '../components/RecordingsSidebar.svelte';

  // Server-side state, refreshed every POLL_MS. Structure:
  //   { mode: 'idle' | 'recording',
  //     buffer: TranscriptEntry[],   bufferBytes, bufferMaxBytes,
  //     activeRecording: { id, name, entries, created_at, duration_ms } | null,
  //     recordings: [{ id, name, entries, duration_ms, ... }],
  //     channelNames: string[],  // display names per Vox slot
  //     sttEnabled: bool, sampleRate: number }
  let state = $state(null);
  let err = $state(null);
  let busy = $state(false);

  // Selected sidebar item: 'live' (rolling buffer), 'active' (in-flight
  // recording), or a recording id (saved). Recording auto-selects to
  // 'active' on start; stop drops back to 'live' unless the user is
  // already looking at a saved one.
  let selected = $state('live');
  let previouslyActive = false;

  // Text field for the "New recording" name prompt shown when the user
  // clicks the + in the sidebar.
  let name = $state('');
  let showNewForm = $state(false);
  let newFormEl = $state(null);

  const POLL_MS = 500;
  let timer = null;

  // Autoscroll bookkeeping for the live transcript div. See onLiveScroll.
  let liveScroller = $state(null);
  let stickToBottom = $state(true);
  const STICK_THRESHOLD = 0.05;
  let lastEntryId = null;
  let lastEntryCount = 0;

  function onLiveScroll(e) {
    const el = e.currentTarget;
    const total = el.scrollHeight - el.clientHeight;
    if (total <= 0) { stickToBottom = true; return; }
    const remaining = (total - el.scrollTop) / total;
    stickToBottom = remaining <= STICK_THRESHOLD;
  }

  async function maybeSnapToBottom() {
    if (!liveScroller || !stickToBottom) return;
    await tick();
    liveScroller.scrollTop = liveScroller.scrollHeight;
  }

  onMount(async () => {
    await refresh();
    timer = setInterval(refresh, POLL_MS);
  });

  onDestroy(() => {
    if (timer) { clearInterval(timer); timer = null; }
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
    // Best-effort: clear the server's playback flag if we were in a
    // playback session when the user navigated away. Fire-and-forget.
    if (playingRecordingId !== null) api.setRecordPlayback(false);
  });

  async function refresh() {
    try {
      state = await api.getRecordState();
      err = null;
    } catch (e) {
      err = e.message;
      return;
    }
    // Auto-select the active recording the first time it appears, so
    // the "New Recording" flow lands the user on the recording view
    // without a manual click.
    const isActive = !!state.activeRecording;
    if (isActive && !previouslyActive) {
      selected = 'active';
    } else if (!isActive && previouslyActive && selected === 'active') {
      // Recording just stopped — drop back to the live buffer if the
      // user was watching the active one.
      selected = 'live';
    }
    previouslyActive = isActive;

    // If the selected saved recording is gone (deleted from another
    // tab), fall back to live rather than showing an empty pane.
    if (
      selected !== 'live' &&
      selected !== 'active' &&
      !state.recordings.some((r) => r.id === selected)
    ) {
      selected = 'live';
    }

    // Autoscroll the transcript pane if we're actually looking at one
    // with live-changing entries (buffer or active bucket).
    const entries = selected === 'active'
      ? state.activeRecording?.entries ?? []
      : selected === 'live'
        ? state.buffer
        : [];
    const latestId = entries.length ? entries[entries.length - 1].id : null;
    if (latestId !== lastEntryId || entries.length !== lastEntryCount) {
      lastEntryId = latestId;
      lastEntryCount = entries.length;
      maybeSnapToBottom();
    }
  }

  async function beginNewRecording() {
    showNewForm = true;
    await tick();
    newFormEl?.focus();
  }
  function cancelNewRecording() {
    showNewForm = false;
    name = '';
  }
  async function submitNewRecording() {
    if (busy) return;
    busy = true;
    try {
      await api.startNewRecording(name.trim());
      name = '';
      showNewForm = false;
      selected = 'active';
      await refresh();
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  async function stopRecording() {
    if (busy || !state?.activeRecording) return;
    busy = true;
    const stoppedId = state.activeRecording.id;
    try {
      await api.stopNewRecording(stoppedId);
      // Land the user on the just-saved recording so they can review
      // the transcript + play back segments without hunting for it.
      selected = stoppedId;
      await refresh();
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  async function discardRecording() {
    if (busy || !state?.activeRecording) return;
    if (!confirm('Discard this recording without saving?')) return;
    busy = true;
    try {
      await api.deleteSavedRecording(state.activeRecording.id);
      selected = 'live';
      await refresh();
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  async function renameRecording(id, name) {
    if (busy) return;
    busy = true;
    try {
      await api.renameSavedRecording(id, name);
      await refresh();
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  async function clearLiveBuffer() {
    if (busy) return;
    busy = true;
    try {
      await api.clearLiveBuffer();
      await refresh();
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  async function deleteSavedFromSidebar(id) {
    if (busy) return;
    busy = true;
    try {
      await api.deleteSavedRecording(id);
      if (selected === id) selected = 'live';
      await refresh();
    } catch (e) {
      err = e.message;
    } finally {
      busy = false;
    }
  }

  function fmtTime(secs) {
    if (!secs) return '';
    const d = new Date(secs * 1000);
    return d.toLocaleString();
  }

  function fmtDur(ms) {
    if (!ms) return '0:00';
    const total = Math.floor(ms / 1000);
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
  }

  function fmtBytes(n) {
    if (n < 1024) return `${n} B`;
    return `${(n / 1024).toFixed(1)} KB`;
  }

  // ---- Row grouping for the columns-per-channel log table ----
  //
  // Entries whose wall-clock ranges overlap or fall within CLUSTER_GAP_MS
  // of each other get merged into one row. A large gap between clusters
  // (≥ SILENCE_ROW_MIN_MS) inserts a silence row with an empty channels
  // area — its master column play button seeks the mixed track to the
  // next speech row's mixedStart, i.e. "skip the silence".
  const CLUSTER_GAP_MS = 500;
  const SILENCE_ROW_MIN_MS = 3000;

  function entryEpochMs(e) {
    return e.start_wall_ms ?? (e.created_at ? e.created_at * 1000 : 0);
  }
  function entryEndMs(e) {
    return entryEpochMs(e) + (e.audio_duration_ms ?? 0);
  }
  function entryMixedEnd(e) {
    return (e.mixed_start_ms ?? 0) + (e.audio_duration_ms ?? 0);
  }

  function computeRows(entries) {
    const sorted = [...(entries ?? [])].sort(
      (a, b) => entryEpochMs(a) - entryEpochMs(b),
    );
    const rows = [];
    let cluster = [];
    let clusterEnd = 0;
    const flush = () => {
      if (!cluster.length) return;
      const wallStart = Math.min(...cluster.map(entryEpochMs));
      const wallEnd = Math.max(...cluster.map(entryEndMs));
      const mixedVals = cluster
        .map((e) => e.mixed_start_ms)
        .filter((v) => v != null);
      const mixedStart = mixedVals.length ? Math.min(...mixedVals) : null;
      const byChannel = {};
      for (const e of cluster) {
        const key = e.channel || 'Unknown';
        (byChannel[key] ??= []).push(e);
      }
      rows.push({
        kind: 'speech',
        wallStart,
        wallEnd,
        mixedStart,
        entries: cluster.slice(),
        byChannel,
      });
      cluster = [];
    };
    for (const e of sorted) {
      const eStart = entryEpochMs(e);
      if (!cluster.length || eStart - clusterEnd <= CLUSTER_GAP_MS) {
        cluster.push(e);
        clusterEnd = Math.max(clusterEnd, entryEndMs(e));
      } else {
        const gap = eStart - clusterEnd;
        const prev = cluster[cluster.length - 1];
        const prevMixedEnd = entryMixedEnd(prev);
        const prevEnd = clusterEnd;
        flush();
        if (gap >= SILENCE_ROW_MIN_MS) {
          rows.push({
            kind: 'silence',
            wallStart: prevEnd,
            wallEnd: eStart,
            mixedStart: prevMixedEnd,
            entries: [],
            byChannel: {},
          });
        }
        cluster = [e];
        clusterEnd = entryEndMs(e);
      }
    }
    flush();
    return rows;
  }

  // Union of channel names in the entries, biased to `canonical` order
  // (from state.channelNames), and with TTS pushed to the end so it
  // reads as "spoken" columns on the left, then the synth column, then
  // Master.
  function extractChannels(entries, canonical = []) {
    const seen = new Set();
    const cols = [];
    for (const c of canonical) {
      if (!seen.has(c)) {
        seen.add(c);
        cols.push(c);
      }
    }
    for (const e of entries ?? []) {
      if (!e.channel || seen.has(e.channel)) continue;
      seen.add(e.channel);
      cols.push(e.channel);
    }
    const tts = cols.indexOf('TTS');
    if (tts >= 0) {
      cols.splice(tts, 1);
      cols.push('TTS');
    }
    return cols;
  }

  // Wall-clock unix ms → "M:SS" relative to the recording's epoch. Used
  // for the leftmost timestamp column in the log table.
  function fmtRelMs(ms, epochMs) {
    const rel = Math.max(0, Math.floor((ms - epochMs) / 1000));
    const m = Math.floor(rel / 60);
    const s = rel % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
  }

  function recordingEpochMs(rec) {
    return rec?.created_at ? rec.created_at * 1000 : 0;
  }

  // Chronological playback state. One shared <audio> element per pane
  // (active + saved) is used so seeking + play is atomic. We track
  // currentTime with a rAF loop so the "playing now" highlight moves
  // smoothly; server-side gets pinged with /record/playback so any
  // VAD-captured vox during playback is flagged (see push_transcript).
  let playingRecordingId = $state(null); // which recording is currently sounding
  let playingTimeMs = $state(0);          // ms — master timeline in 'master' mode, clip offset in 'clip' mode
  let masterAudioEl = $state(null);       // hidden <audio> for the silence-gated mixed track
  let clipAudioEl = $state(null);         // hidden <audio> for one-shot per-entry segment playback
  // 'master' when the mixed track is playing (auto-advances through rows);
  // 'clip' when a single per-channel utterance is playing (no advance).
  // null when nothing is playing. Drives which element the rAF loop
  // samples and which row/clip button lights up.
  let playbackMode = $state(null);
  // Which entry id is playing under 'clip' mode. Only set when
  // playbackMode === 'clip'.
  let playingClipId = $state(null);
  let rafId = 0;

  // ---- Master vs clip playback ----
  //
  // Master: click the ▶ in a row's Master column. Seeks the (hidden)
  // mixed-track element to that row's `mixedStart` and lets it play
  // straight through — subsequent rows highlight naturally as the
  // playhead crosses their mixedStart boundaries. Silence rows have
  // mixedStart == prev row's mixed_end, so continuous master playback
  // skips right past them.
  //
  // Clip: click a per-channel cell's ▶. Fetches just that entry's
  // audio segment via /record/recordings/:id/segment and plays it in a
  // second hidden element. No auto-advance — one utterance, done.
  //
  // A silence row's ▶ seeks master to the next speech row's mixedStart
  // and plays from there ("skip the silence"). Last-row silence just
  // stops.

  function tickPlayhead() {
    const el = playbackMode === 'clip' ? clipAudioEl : masterAudioEl;
    if (el && !el.paused) {
      playingTimeMs = el.currentTime * 1000;
    }
    rafId = requestAnimationFrame(tickPlayhead);
  }

  function ensureRaf() {
    if (!rafId) rafId = requestAnimationFrame(tickPlayhead);
  }

  async function playMasterAtMs(recordingId, startMs) {
    await tick();
    if (!masterAudioEl) return;
    const wantSrc = api.recordingMixedUrl(recordingId);
    if (!masterAudioEl.src.endsWith(wantSrc)) {
      masterAudioEl.src = wantSrc;
      masterAudioEl.load();
    }
    if (clipAudioEl && !clipAudioEl.paused) clipAudioEl.pause();
    playbackMode = 'master';
    playingClipId = null;
    playingRecordingId = recordingId;
    playingTimeMs = startMs;
    const seekAndPlay = () => {
      try { masterAudioEl.currentTime = Math.max(0, startMs) / 1000; } catch {}
      masterAudioEl.play().catch((e) => console.warn('master play failed', e));
    };
    if (masterAudioEl.readyState >= 1) {
      seekAndPlay();
    } else {
      masterAudioEl.addEventListener('loadedmetadata', seekAndPlay, { once: true });
    }
    ensureRaf();
    api.setRecordPlayback(true);
  }

  /// Master-column click for a row. Speech rows seek to their own
  /// `mixedStart`; silence rows jump to the next speech row's
  /// `mixedStart` so the user can use the master column as a
  /// scrub-forward affordance without waiting through dead air.
  function playMasterFromRow(row, rows, recordingId) {
    if (row.kind === 'speech') {
      if (row.mixedStart != null) playMasterAtMs(recordingId, row.mixedStart);
      return;
    }
    // Silence row: find the next speech row and jump to its mixedStart.
    const idx = rows.indexOf(row);
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].mixedStart != null) {
        playMasterAtMs(recordingId, rows[i].mixedStart);
        return;
      }
    }
    // No later speech row: nothing to play — just stop.
    stopPlayback();
  }

  /// One-shot playback of a single utterance segment. Uses the segment
  /// endpoint so we get an isolated mini-WAV rather than seeking a big
  /// track. TTS entries have their own `audio_url` (the cached widget
  /// clip) — same element, different source.
  async function playClip(entry, recordingId) {
    await tick();
    if (!clipAudioEl) return;
    let src;
    if (entry.audio_url) {
      src = entry.audio_url;
    } else if (entry.audio_start_ms != null && entry.audio_duration_ms != null) {
      src = api.recordingSegmentUrl(
        recordingId,
        entry.audio_start_ms,
        entry.audio_duration_ms,
      );
    } else {
      return;
    }
    clipAudioEl.src = src;
    clipAudioEl.load();
    if (masterAudioEl && !masterAudioEl.paused) masterAudioEl.pause();
    playbackMode = 'clip';
    playingClipId = entry.id;
    playingRecordingId = recordingId;
    playingTimeMs = 0;
    clipAudioEl.play().catch((e) => console.warn('clip play failed', e));
    ensureRaf();
    api.setRecordPlayback(true);
  }

  function stopPlayback() {
    if (masterAudioEl && !masterAudioEl.paused) masterAudioEl.pause();
    if (clipAudioEl && !clipAudioEl.paused) clipAudioEl.pause();
    playingRecordingId = null;
    playingClipId = null;
    playingTimeMs = 0;
    playbackMode = null;
    api.setRecordPlayback(false);
  }

  // ---- Progress predicates for the swipe animation ----

  function isClipPlayingNow(entry) {
    return playbackMode === 'clip' && playingClipId === entry.id;
  }
  function clipProgress(entry) {
    if (!isClipPlayingNow(entry)) return 0;
    const dur = entry.audio_duration_ms;
    if (!dur) return 0;
    return Math.max(0, Math.min(1, playingTimeMs / dur));
  }

  /// A speech row is "master-playing" when the mixed-track playhead is
  /// within `[row.mixedStart, next-speech-row.mixedStart)`. Silence rows
  /// have zero-length windows so this is always false for them; the
  /// visual jumps directly from one speech row to the next during
  /// continuous master playback.
  function isRowMasterPlaying(row, rows) {
    if (playbackMode !== 'master' || row.kind !== 'speech') return false;
    if (row.mixedStart == null) return false;
    const idx = rows.indexOf(row);
    let end = Infinity;
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].mixedStart != null) {
        end = rows[i].mixedStart;
        break;
      }
    }
    return playingTimeMs >= row.mixedStart && playingTimeMs < end;
  }
  function rowMasterProgress(row, rows) {
    if (!isRowMasterPlaying(row, rows)) return 0;
    const idx = rows.indexOf(row);
    let end = null;
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].mixedStart != null) {
        end = rows[i].mixedStart;
        break;
      }
    }
    if (end == null) {
      // Last speech row — no successor to bound the range against.
      // Fall back to the row's own wall-clock duration as the master
      // slice length; caps progress at 1.0 once currentTime overshoots.
      const dur = Math.max(1, row.wallEnd - row.wallStart);
      const elapsed = playingTimeMs - row.mixedStart;
      return Math.max(0, Math.min(1, elapsed / dur));
    }
    const elapsed = playingTimeMs - row.mixedStart;
    const dur = Math.max(1, end - row.mixedStart);
    return Math.max(0, Math.min(1, elapsed / dur));
  }

  function onPlaybackEnded() {
    playingRecordingId = null;
    playingTimeMs = 0;
    api.setRecordPlayback(false);
  }

  function onPlaybackPause() {
    // User hit the native controls' pause. Clear playback flag but
    // keep the currently-highlighted position so they can resume.
    api.setRecordPlayback(false);
  }

  function onPlaybackPlay() {
    api.setRecordPlayback(true);
    ensureRaf();
  }

  // Reset playback whenever the selected recording changes so a stale
  // highlight from a previous pane doesn't linger.
  $effect(() => {
    // Reference `selected` to make this effect track it.
    selected;
    if (playingRecordingId !== null && playingRecordingId !== selected && playingRecordingId !== 'active') {
      stopPlayback();
    }
  });

  // Click-to-correct transcript entries. One entry is editable at a time
  // (across all three panes) so the UI never has to reason about parallel
  // edits. Enter/blur commits, Escape cancels. The edit runs optimistically
  // — local state is updated immediately for feel, then reconciled with
  // the server response on the next refresh.
  let editingEntryId = $state(null);
  let editText = $state('');
  let editInputEl = $state(null);
  let editSaving = $state(false);

  async function beginEditEntry(entry) {
    if (editSaving) return;
    editingEntryId = entry.id;
    editText = entry.text;
    await tick();
    editInputEl?.focus();
    editInputEl?.select();
  }

  function cancelEditEntry() {
    editingEntryId = null;
    editText = '';
  }

  // `recordingId` = null for entries in the live buffer or the active
  // (in-flight) recording — those live in RAM. Pass the recording id for
  // entries in a saved recording so we hit the sqlite-backed endpoint.
  async function commitEditEntry(entry, recordingId) {
    if (editingEntryId !== entry.id || editSaving) return;
    const next = editText.trim();
    // Blank cancels rather than deleting — that's what the server would
    // reject anyway, and it matches the "clicked by mistake" intent.
    if (!next || next === entry.text) {
      cancelEditEntry();
      return;
    }
    editSaving = true;
    const prev = entry.text;
    entry.text = next; // optimistic
    try {
      if (recordingId) {
        await api.updateSavedRecordingEntry(recordingId, entry.id, next);
      } else {
        await api.updateLiveRecordingEntry(entry.id, next);
      }
      editingEntryId = null;
      editText = '';
      await refresh();
    } catch (e) {
      entry.text = prev; // revert
      err = `couldn't save edit: ${e.message}`;
    } finally {
      editSaving = false;
    }
  }

  function onEditKey(e, entry, recordingId) {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitEditEntry(entry, recordingId);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelEditEntry();
    }
  }

  // Inline rename for the in-flight recording's pane header. Click the
  // name to enter edit mode; Enter/blur commits, Escape cancels. Uses
  // the same PATCH endpoint as saved recordings — the server routes
  // active-id patches to RAM state, saved-id patches to sqlite.
  let renamingActive = $state(false);
  let activeRenameText = $state('');
  let activeRenameEl = $state(null);

  async function beginActiveRename() {
    if (!state?.activeRecording) return;
    activeRenameText = state.activeRecording.name;
    renamingActive = true;
    await tick();
    activeRenameEl?.focus();
    activeRenameEl?.select();
  }

  async function commitActiveRename() {
    if (!renamingActive || !state?.activeRecording) return;
    renamingActive = false;
    const name = activeRenameText.trim();
    if (!name || name === state.activeRecording.name) return;
    await renameRecording(state.activeRecording.id, name);
  }

  function cancelActiveRename() {
    renamingActive = false;
  }

  function onActiveRenameKey(e) {
    if (e.key === 'Enter') { e.preventDefault(); commitActiveRename(); }
    else if (e.key === 'Escape') { e.preventDefault(); cancelActiveRename(); }
  }

  const isRecording = $derived(state?.mode === 'recording');
  const bufferPct = $derived(
    state ? Math.min(100, (state.bufferBytes / state.bufferMaxBytes) * 100) : 0,
  );
  // Currently-selected saved recording (when `selected` is an id).
  const selectedSaved = $derived(
    state && selected !== 'live' && selected !== 'active'
      ? state.recordings.find((r) => r.id === selected) ?? null
      : null,
  );
  const channelFallback = $derived(state?.channelNames?.[0] ?? 'Vox 1');
</script>

{#snippet hiddenAudios()}
  <!-- Both audio elements are hidden — playback is driven entirely by
       the ▶ buttons in the log table. Master (mixed track) auto-advances
       through subsequent rows; Clip (per-entry segment) is one-shot. -->
  <audio
    bind:this={masterAudioEl}
    onplay={onPlaybackPlay}
    onpause={onPlaybackPause}
    onended={onPlaybackEnded}
    preload="none"
  ></audio>
  <audio
    bind:this={clipAudioEl}
    onplay={onPlaybackPlay}
    onpause={onPlaybackPause}
    onended={onPlaybackEnded}
    preload="none"
  ></audio>
{/snippet}

{#snippet playbackBar()}
  {#if playingRecordingId != null}
    <div class="playback-bar">
      <span class="playhead">
        ▶ {playbackMode === 'master' ? 'master' : 'clip'} — {fmtDur(playingTimeMs)}
      </span>
      <button type="button" class="ghost" onclick={stopPlayback}>Stop playback</button>
    </div>
  {/if}
{/snippet}

{#snippet logTable(recording, canonicalChannels)}
  {@const rows = computeRows(recording.entries)}
  {@const channels = extractChannels(recording.entries, canonicalChannels)}
  {@const epoch = recordingEpochMs(recording)}
  {@const recId = recording.id}
  <div class="log-scroller" bind:this={liveScroller} onscroll={onLiveScroll}>
    <table class="log-table">
      <thead>
        <tr>
          <th class="col-ts">time</th>
          {#each channels as ch (ch)}
            <th class="col-ch" class:tts-col={ch === 'TTS'}>{ch}</th>
          {/each}
          <th class="col-master">Master</th>
        </tr>
      </thead>
      <tbody>
        {#each rows as row, i (row.kind + ':' + row.wallStart)}
          <tr class:silence={row.kind === 'silence'}>
            <td class="col-ts">
              {#if row.kind === 'silence'}
                <span class="silence-tag">silence · {Math.round((row.wallEnd - row.wallStart) / 1000)}s</span>
              {:else}
                {fmtRelMs(row.wallStart, epoch)} – {fmtRelMs(row.wallEnd, epoch)}
              {/if}
            </td>
            {#each channels as ch (ch)}
              <td class="col-ch">
                {#each row.byChannel[ch] ?? [] as e (e.id)}
                  {@render clipCell(e, ch, recId)}
                {/each}
              </td>
            {/each}
            <td class="col-master">
              {@render masterCell(row, rows, recId)}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/snippet}

{#snippet clipCell(entry, channel, recId)}
  <div
    class="clip-btn"
    class:playing={isClipPlayingNow(entry)}
    class:echo={entry.during_playback && !entry.audio_url}
    class:tts={channel === 'TTS'}
  >
    {#if isClipPlayingNow(entry)}
      <div class="clip-fill" style="width: {clipProgress(entry) * 100}%" aria-hidden="true"></div>
    {/if}
    {#if entry.speaker}
      <span class="speaker-tag" title="Speaker attribution">{entry.speaker}</span>
    {/if}
    {#if entry.during_playback && !entry.audio_url}
      <span class="echo-hint" title="Captured while playback was active — likely mic echo, not live speech.">echo?</span>
    {/if}
    {#if editingEntryId === entry.id}
      <input
        class="clip-edit"
        type="text"
        bind:this={editInputEl}
        bind:value={editText}
        disabled={editSaving}
        onkeydown={(ev) => onEditKey(ev, entry, recId === state?.activeRecording?.id ? null : recId)}
        onblur={() => commitEditEntry(entry, recId === state?.activeRecording?.id ? null : recId)}
        aria-label="Correct transcript"
      />
    {:else}
      <button
        type="button"
        class="clip-text"
        title="Click to play · double-click to edit"
        onclick={() => playClip(entry, recId)}
        ondblclick={() => beginEditEntry(entry)}
      >{entry.text}</button>
    {/if}
  </div>
{/snippet}

{#snippet masterCell(row, rows, recId)}
  <button
    type="button"
    class="master-btn"
    class:playing={isRowMasterPlaying(row, rows)}
    class:silence={row.kind === 'silence'}
    onclick={() => playMasterFromRow(row, rows, recId)}
    title={row.kind === 'silence' ? 'Skip silence — jump master to next row' : 'Play the mixed audio from here'}
  >
    {#if isRowMasterPlaying(row, rows)}
      <div class="clip-fill" style="width: {rowMasterProgress(row, rows) * 100}%" aria-hidden="true"></div>
    {/if}
    <span class="master-glyph">▶</span>
  </button>
{/snippet}

<div class="record-shell">
  <RecordingsSidebar
    recordings={state?.recordings ?? []}
    activeRecording={state?.activeRecording ?? null}
    {selected}
    onSelect={(k) => { selected = k; showNewForm = false; }}
    onNewRecording={beginNewRecording}
    onRename={renameRecording}
    onDelete={deleteSavedFromSidebar}
    onClearLive={clearLiveBuffer}
  />

  <div class="main">
    {#if err}
      <div class="err">{err}</div>
    {/if}

    {#if !state}
      <div class="empty">loading…</div>
    {:else if showNewForm}
      <form class="new-recording" onsubmit={(e) => { e.preventDefault(); submitNewRecording(); }}>
        <label for="new-recording-name">Name for this recording (optional)</label>
        <input
          id="new-recording-name"
          type="text"
          bind:this={newFormEl}
          bind:value={name}
          disabled={busy}
          placeholder="e.g. Session 3 — the tomb"
          onkeydown={(e) => { if (e.key === 'Escape') { e.preventDefault(); cancelNewRecording(); } }}
        />
        <div class="new-actions">
          <button type="button" class="ghost" onclick={cancelNewRecording} disabled={busy}>Cancel</button>
          <button type="submit" class="primary" disabled={busy}>
            {busy ? 'Starting…' : 'Start Recording'}
          </button>
        </div>
      </form>
    {:else if selected === 'live'}
      <!-- Rolling ephemeral buffer view. -->
      <div class="pane-head">
        <h1>Ephemeral live transcript (buffered; not recorded)</h1>
        <div class="hint">
          background live transcription from the Vox channel · trims oldest
          entries once the buffer hits {fmtBytes(state.bufferMaxBytes)}
          {#if !state.sttEnabled}
            <span class="stt-off"> · STT disabled</span>
          {/if}
        </div>
      </div>
      <div class="buf-meter" title="{fmtBytes(state.bufferBytes)} of {fmtBytes(state.bufferMaxBytes)}">
        <div class="buf-fill" style="width: {bufferPct}%"></div>
      </div>
      {#if state.buffer.length === 0}
        <div class="empty pane-empty">
          nothing spoken yet — the {channelFallback} channel is quiet
        </div>
      {:else}
        <ul
          class="entries"
          bind:this={liveScroller}
          onscroll={onLiveScroll}
        >
          {#each state.buffer as e (e.id)}
            <li>
              <span class="channel-tag">{e.channel || channelFallback}</span>
              <span class="ts">{fmtTime(e.created_at)}</span>
              {#if editingEntryId === e.id}
                <input
                  class="text-edit"
                  type="text"
                  bind:this={editInputEl}
                  bind:value={editText}
                  disabled={editSaving}
                  onkeydown={(ev) => onEditKey(ev, e, null)}
                  onblur={() => commitEditEntry(e, null)}
                  aria-label="Correct transcript"
                />
              {:else}
                <button
                  type="button"
                  class="text text-btn"
                  title="Click to correct this transcript"
                  onclick={() => beginEditEntry(e)}
                >{e.text}</button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    {:else if selected === 'active' && state.activeRecording}
      <!-- Live view of the in-flight recording bucket. Name is
           click-to-rename; server routes the PATCH to the active
           RAM state so the change is instant and persists on Stop. -->
      <div class="pane-head active">
        <div class="active-title">
          <div class="rec-badge" aria-hidden="true">
            <span class="dot"></span>REC
          </div>
          {#if renamingActive}
            <input
              class="active-rename"
              type="text"
              bind:this={activeRenameEl}
              bind:value={activeRenameText}
              onkeydown={onActiveRenameKey}
              onblur={commitActiveRename}
              aria-label="Rename active recording"
            />
          {:else}
            <button
              type="button"
              class="active-name-btn"
              title="Click to rename this recording"
              onclick={beginActiveRename}
            >{state.activeRecording.name}</button>
          {/if}
          <span class="active-time">{fmtDur(state.activeRecording.duration_ms ?? 0)}</span>
        </div>
        <div class="active-actions">
          <button type="button" class="danger" disabled={busy} onclick={discardRecording}>
            Discard
          </button>
          <button type="button" class="primary" disabled={busy} onclick={stopRecording}>
            Stop &amp; Save
          </button>
        </div>
      </div>
      {@render playbackBar()}
      {@render hiddenAudios()}
      {#if state.activeRecording.entries.length === 0}
        <div class="empty pane-empty">listening…</div>
      {:else}
        {@render logTable(state.activeRecording, state.channelNames ?? [])}
      {/if}
    {:else if selectedSaved}
      <!-- Saved recording detail. -->
      <div class="pane-head">
        <h1>{selectedSaved.name}</h1>
        <div class="hint">
          {fmtTime(selectedSaved.created_at)} · {fmtDur(selectedSaved.duration_ms)}
          · {selectedSaved.entries.length} utterances
        </div>
        <a
          class="download-btn"
          href={api.recordingArchiveUrl(selectedSaved.id)}
          download
          title="Download a zip with transcript.json, mixed.wav, audio.wav, and per-utterance clips"
        >⤓ Download archive</a>
      </div>
      {@render playbackBar()}
      {@render hiddenAudios()}
      {#if selectedSaved.entries.length === 0}
        <div class="empty pane-empty">no transcript for this recording</div>
      {:else}
        {@render logTable(selectedSaved, state.channelNames ?? [])}
      {/if}
    {:else}
      <div class="empty pane-empty">Select a recording from the sidebar.</div>
    {/if}
  </div>
</div>

<style>
  .record-shell {
    display: flex;
    align-items: stretch;
    height: calc(100vh - 46px);
    min-height: 0;
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

  .pane-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
  }
  .pane-head h1 { font-size: 20px; margin: 0; }
  .hint { color: var(--muted); font-size: 12px; }
  .stt-off { color: var(--err); font-weight: 500; }

  .empty { color: var(--muted); font-size: 13px; padding: 6px 0; }
  .pane-empty { padding: 24px 0; text-align: center; }
  .err {
    color: var(--err);
    background: rgba(255, 92, 92, 0.08);
    border: 1px solid rgba(255, 92, 92, 0.35);
    padding: 8px 12px;
    border-radius: 6px;
    font-size: 12px;
  }

  .new-recording {
    display: flex;
    flex-direction: column;
    gap: 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 20px;
    max-width: 520px;
    margin: auto;
  }
  .new-recording label { color: var(--muted); font-size: 12px; }
  .new-recording input[type="text"] {
    background: rgba(0,0,0,0.2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 14px;
    font-family: inherit;
  }
  .new-actions { display: flex; justify-content: flex-end; gap: 8px; }

  .pane-head.active {
    padding: 10px 14px;
    background: rgba(255, 80, 80, 0.06);
    border: 1px solid rgba(255, 80, 80, 0.4);
    border-radius: 10px;
  }
  .active-title { display: flex; align-items: center; gap: 12px; min-width: 0; }
  .active-name-btn {
    background: transparent;
    border: none;
    color: var(--text);
    font: inherit;
    font-size: 18px;
    font-weight: 600;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40ch;
  }
  .active-name-btn:hover { background: rgba(255,255,255,0.05); }
  .active-rename {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid rgba(255, 80, 80, 0.6);
    border-radius: 4px;
    padding: 2px 8px;
    font: inherit;
    font-size: 18px;
    font-weight: 600;
    min-width: 20ch;
    max-width: 40ch;
  }
  .active-time { color: var(--muted); font-variant-numeric: tabular-nums; font-size: 13px; }
  .active-actions { display: flex; gap: 8px; flex-shrink: 0; }
  .rec-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: var(--err);
    color: #0b0d10;
    font-weight: 700;
    font-size: 11px;
    letter-spacing: 0.08em;
    padding: 3px 8px;
    border-radius: 4px;
  }
  .rec-badge .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
    opacity: 0.8;
    animation: rec-pulse 1.2s ease-in-out infinite;
  }
  @keyframes rec-pulse {
    0%, 100% { opacity: 0.55; transform: scale(0.9); }
    50%      { opacity: 1;    transform: scale(1.15); }
  }

  .buf-meter {
    width: 100%;
    max-width: 320px;
    height: 6px;
    border-radius: 3px;
    background: rgba(0,0,0,0.3);
    overflow: hidden;
  }
  .buf-fill {
    height: 100%;
    background: linear-gradient(to right, #2ecc4a, #f2c94c 80%, #ff5c5c 100%);
    transition: width 0.2s linear;
  }

  .download-btn {
    background: transparent;
    color: var(--accent);
    border: 1px solid rgba(122,162,255,0.4);
    padding: 4px 12px;
    font-size: 12px;
    font-weight: 600;
    border-radius: 4px;
    text-decoration: none;
    cursor: pointer;
    white-space: nowrap;
  }
  .download-btn:hover { background: rgba(122,162,255,0.12); }

  ul.entries {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
    overflow-y: auto;
    scrollbar-gutter: stable;
    min-height: 0;
    flex: 1;
  }
  ul.entries li {
    display: grid;
    grid-template-columns: max-content max-content 1fr;
    grid-template-rows: auto auto;
    gap: 4px 10px;
    padding: 6px 8px;
    border-radius: 4px;
    background: rgba(0,0,0,0.15);
    align-items: baseline;
    position: relative;
    overflow: hidden;
  }
  .channel-tag {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--accent);
    background: rgba(122,162,255,0.12);
    padding: 2px 6px;
    border-radius: 3px;
    white-space: nowrap;
  }
  .speaker-tag {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.02em;
    color: #b7f0c4;
    background: rgba(46, 204, 74, 0.15);
    padding: 2px 6px;
    border-radius: 3px;
    white-space: nowrap;
  }
  .playback-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    background: rgba(122,162,255,0.12);
    border: 1px solid rgba(122,162,255,0.35);
    border-radius: 6px;
    font-size: 12px;
    color: var(--text);
  }
  .playhead {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--accent);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .ts {
    color: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .text {
    font-size: 13px;
    line-height: 1.4;
    word-break: break-word;
  }
  .text-btn {
    background: transparent;
    color: var(--text);
    border: 1px dashed transparent;
    padding: 2px 6px;
    margin: -2px -6px;
    font: inherit;
    font-size: 13px;
    line-height: 1.4;
    text-align: left;
    cursor: text;
    border-radius: 3px;
    grid-column: 3;
    min-width: 0;
    word-break: break-word;
    white-space: normal;
  }
  .text-btn:hover { border-color: rgba(122,162,255,0.4); background: rgba(122,162,255,0.06); }
  .text-edit {
    grid-column: 3;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid rgba(122,162,255,0.6);
    border-radius: 4px;
    padding: 3px 6px;
    font: inherit;
    font-size: 13px;
    line-height: 1.4;
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }

  button {
    padding: 6px 14px;
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-weight: 600;
    font-size: 12px;
    cursor: pointer;
  }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  button:hover:not(:disabled) { background: rgba(255,255,255,0.05); }
  button.primary {
    background: rgba(122,162,255,0.2);
    color: var(--text);
    border-color: rgba(122,162,255,0.6);
  }
  button.primary:hover:not(:disabled) { background: rgba(122,162,255,0.3); }
  button.danger {
    background: transparent;
    color: var(--err);
    border-color: rgba(255,92,92,0.5);
  }
  button.danger:hover:not(:disabled) { background: rgba(255,92,92,0.12); }
  button.ghost {
    background: transparent;
    color: var(--muted);
    border-color: var(--border);
  }

  /* ---- Log table (columns-per-channel view) ---- */
  .log-scroller {
    flex: 1;
    min-height: 0;
    overflow: auto;
    scrollbar-gutter: stable;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  .log-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }
  .log-table thead {
    position: sticky;
    top: 0;
    background: var(--panel);
    z-index: 2;
  }
  .log-table th {
    text-align: left;
    padding: 8px 10px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    font-weight: 600;
    border-bottom: 1px solid var(--border);
  }
  .log-table th.tts-col { color: #f2c94c; }
  .log-table td {
    padding: 6px 8px;
    vertical-align: top;
    border-bottom: 1px solid rgba(255,255,255,0.03);
  }
  .log-table tbody tr:hover td { background: rgba(255,255,255,0.02); }
  .log-table tbody tr.silence td { opacity: 0.65; }
  .col-ts {
    width: 110px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .col-ch { min-width: 120px; }
  .col-master {
    width: 56px;
    text-align: center;
    padding-right: 12px;
  }
  .silence-tag {
    display: inline-block;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    background: rgba(255,255,255,0.05);
    padding: 2px 6px;
    border-radius: 3px;
  }

  /* Clip cell — one per entry, stacked when a row has multiple entries
     from the same channel. The text button is the primary click target
     (click = play, dblclick = edit). */
  .clip-btn {
    position: relative;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 6px;
    margin-bottom: 3px;
    border-radius: 4px;
    background: rgba(0,0,0,0.2);
    border: 1px solid rgba(255,255,255,0.04);
    overflow: hidden;
  }
  .clip-btn:last-child { margin-bottom: 0; }
  .clip-btn.playing {
    border-color: rgba(122,162,255,0.6);
    box-shadow: 0 0 0 1px rgba(122,162,255,0.35);
  }
  .clip-btn.tts { background: rgba(255,204,102,0.06); }
  .clip-btn.echo { opacity: 0.55; }
  /* Animated fill: absolute overlay behind the button content whose
     width is bound to per-clip playback progress and updated per rAF
     tick. Content is promoted with `position: relative` to sit above. */
  .clip-fill {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    background: linear-gradient(
      to right,
      rgba(122,162,255,0.28),
      rgba(122,162,255,0.42)
    );
    border-right: 1px solid rgba(122,162,255,0.75);
    pointer-events: none;
    z-index: 0;
  }
  .clip-btn > *:not(.clip-fill) {
    position: relative;
    z-index: 1;
  }
  .clip-text {
    flex: 1;
    background: transparent;
    color: var(--text);
    border: none;
    padding: 0;
    font: inherit;
    font-size: 12px;
    line-height: 1.4;
    text-align: left;
    cursor: pointer;
    min-width: 0;
    word-break: break-word;
    white-space: normal;
  }
  .clip-text:hover { color: var(--accent); }
  .clip-edit {
    flex: 1;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid rgba(122,162,255,0.6);
    border-radius: 3px;
    padding: 2px 4px;
    font: inherit;
    font-size: 12px;
    line-height: 1.4;
    min-width: 0;
    width: 100%;
    box-sizing: border-box;
  }
  .echo-hint {
    font-size: 10px;
    color: #ff9a5c;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    background: rgba(255,154,92,0.15);
    padding: 1px 4px;
    border-radius: 3px;
  }

  /* Master cell — the ▶ that plays the mixed audio from this row's
     mixedStart onward (auto-advances through remaining rows). Uses the
     same animated fill machinery as clip buttons. */
  .master-btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 26px;
    padding: 0;
    background: rgba(122,162,255,0.12);
    color: var(--accent);
    border: 1px solid rgba(122,162,255,0.4);
    border-radius: 4px;
    cursor: pointer;
    overflow: hidden;
  }
  .master-btn:hover { background: rgba(122,162,255,0.22); }
  .master-btn.silence {
    background: rgba(255,255,255,0.03);
    border-color: rgba(255,255,255,0.1);
    color: var(--muted);
  }
  .master-btn.playing {
    border-color: rgba(122,162,255,0.75);
    box-shadow: 0 0 0 1px rgba(122,162,255,0.35);
  }
  .master-btn > *:not(.clip-fill) {
    position: relative;
    z-index: 1;
  }
  .master-glyph { font-size: 12px; }
</style>
