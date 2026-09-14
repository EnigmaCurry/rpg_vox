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

  const POLL_MS = 500;
  let timer = null;

  // Autoscroll — targets the *page* (browser window scrollbar), because
  // the Record layout is designed so the page itself is the only
  // scroller.
  //
  // Design:
  //   * `stickToBottom` — did the user's last scroll leave them near the
  //     bottom? Recomputed on every scroll event, so scrollbar drags and
  //     touch flings are captured just like wheel/keyboard.
  //   * ResizeObserver on <body> — the *only* trigger for a snap. Fires
  //     exactly when scrollable content grows (new log rows, cell
  //     reflow, viewport resize). No rAF heartbeat: constantly writing
  //     scrollTop every frame overrode manual scroll attempts before the
  //     browser could settle them, which is why the scrollbar felt
  //     un-draggable.
  //   * `cooldownEnd` — after any user-initiated scroll, RO snaps are
  //     paused for a short window so a mid-scroll transcript growth
  //     doesn't yank the page back to the bottom under the user's
  //     finger.
  //   * `lastWriteTarget` — remembers what scrollTop we most recently
  //     wrote so the corresponding scroll event can be distinguished
  //     from a genuine user scroll (which is what starts the cooldown).
  //
  // Threshold is a pixel distance — "well clear of the bottom" should
  // mean the same movement regardless of how much history has piled up.
  let stickToBottom = $state(true);
  const STICK_THRESHOLD_PX = 200;
  const USER_SCROLL_COOLDOWN_MS = 800;
  let cooldownEnd = 0;
  let lastWriteTarget = -1;

  function docMetrics() {
    const doc = document.scrollingElement || document.documentElement;
    return {
      doc,
      scrollTop: doc.scrollTop,
      scrollHeight: doc.scrollHeight,
      clientHeight: doc.clientHeight,
    };
  }

  function onWindowScroll() {
    const { scrollTop, scrollHeight, clientHeight } = docMetrics();
    const total = scrollHeight - clientHeight;
    const isProgrammatic = Math.abs(scrollTop - lastWriteTarget) < 2;
    // Consume the pending target so a later user-initiated scroll to a
    // different position doesn't get misread as programmatic.
    lastWriteTarget = -1;
    if (!isProgrammatic) {
      cooldownEnd = performance.now() + USER_SCROLL_COOLDOWN_MS;
    }
    stickToBottom = total <= 0 || (total - scrollTop) <= STICK_THRESHOLD_PX;
  }

  function snapToBottom() {
    const { doc, scrollHeight, clientHeight } = docMetrics();
    const target = scrollHeight - clientHeight;
    if (target <= 0) return;
    lastWriteTarget = target;
    doc.scrollTop = target;
  }

  function isFollowingPane() {
    // Auto-pin behavior is meaningful only where new content keeps
    // arriving — the ephemeral live buffer and the in-flight active
    // recording. Saved recordings are frozen; yanking the user to the
    // end of a completed transcript isn't useful.
    return selected === 'live' || selected === 'active';
  }

  $effect(() => {
    window.addEventListener('scroll', onWindowScroll, { passive: true });
    const ro = new ResizeObserver(() => {
      // Skip the snap for a beat after any user scroll gesture so
      // dragging/wheel/touch stays under the user's control.
      if (performance.now() < cooldownEnd) return;
      // Playback drives its own scroll (bringing the current clip into
      // view); an autoscroll-to-bottom during playback would fight it
      // and yank the user away from what's playing.
      if (playbackMode != null) return;
      if (!isFollowingPane()) return;
      if (stickToBottom) snapToBottom();
    });
    ro.observe(document.body);
    // Initial snap if we mount on a following pane with content already
    // past the viewport.
    queueMicrotask(() => {
      if (!isFollowingPane()) return;
      if (stickToBottom) snapToBottom();
    });
    return () => {
      window.removeEventListener('scroll', onWindowScroll);
      ro.disconnect();
    };
  });

  // Navigation intent: switching to live/active means "I want to follow
  // the current transcription" — re-arm the bottom-pin. Switching to a
  // saved recording means "I'm reviewing something finished" — start at
  // the top of its log and disable the pin.
  $effect(() => {
    const cur = selected;
    if (cur === 'live' || cur === 'active') {
      stickToBottom = true;
    } else {
      stickToBottom = false;
      queueMicrotask(() => { window.scrollTo({ top: 0, behavior: 'auto' }); });
    }
  });

  onMount(async () => {
    await refresh();
    timer = setInterval(refresh, POLL_MS);
  });

  onDestroy(() => {
    if (timer) { clearInterval(timer); timer = null; }
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
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
  }

  // Start recording immediately with the server-assigned default name
  // ("Recording <timestamp>"). The user can click-to-rename in the
  // active pane header once the bucket is live — no prompt in the way
  // between "I want to record" and "recording is happening".
  async function beginNewRecording() {
    if (busy) return;
    busy = true;
    try {
      await api.startNewRecording('');
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
  // Terse form for the linear-log timestamp line: just HH:MM. The full
  // date + time from `fmtTime` sits in the row's `title` tooltip so
  // hovering still exposes the exact moment when needed.
  function fmtTimeCompact(secs) {
    if (!secs) return '';
    const d = new Date(secs * 1000);
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  }
  // UTC-only compact time (HH:MMZ). Used in the recording linear-log
  // avatar so the recorded moment is unambiguous across time zones —
  // handy when a session is shared with people elsewhere or the same
  // recording is reviewed on a machine in a different locale.
  function fmtTimeUtcCompact(secs) {
    if (!secs) return '';
    const iso = new Date(secs * 1000).toISOString();
    return `${iso.slice(11, 16)}Z`;
  }
  // Single-letter placeholder derived from the pill text so an avatar
  // that isn't yet wired to a real portrait still reads as
  // recognizably that voice/channel. Trims leading punctuation so a
  // profile like " · echo" doesn't render as "·".
  function initialFrom(label) {
    if (!label) return '?';
    const clean = label.trim().replace(/^[^\p{L}\p{N}]+/u, '');
    const first = clean.charAt(0);
    return first ? first.toUpperCase() : '?';
  }
  // Stable HSL background from any string. Same voice profile always
  // gets the same tint across sessions, and two different profiles
  // reliably render as distinct colors — quick visual differentiation
  // in a long transcript without needing per-character metadata.
  function avatarTint(label) {
    if (!label) return 'hsl(210 20% 30%)';
    let h = 0;
    for (let i = 0; i < label.length; i++) {
      h = (h * 31 + label.charCodeAt(i)) >>> 0;
    }
    return `hsl(${h % 360} 45% 32%)`;
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
  // Every entry is its own row: same-channel repeats always start a new
  // row so no cell ever stacks multiple clips. Entries from *different*
  // channels that fall within CLUSTER_GAP_MS may still share a row, so
  // overlapping speakers stay visually aligned. A gap ≥ SILENCE_ROW_MIN_MS
  // between rows inserts a silence row with an empty channels area — its
  // master column play button seeks the mixed track to the next speech
  // row's audioStart, i.e. "skip the silence".
  const CLUSTER_GAP_MS = 500;
  const SILENCE_ROW_MIN_MS = 3000;

  function entryEpochMs(e) {
    return e.start_wall_ms ?? (e.created_at ? e.created_at * 1000 : 0);
  }
  function entryEndMs(e) {
    return entryEpochMs(e) + (e.audio_duration_ms ?? 0);
  }
  function computeRows(entries) {
    const sorted = [...(entries ?? [])].sort(
      (a, b) => entryEpochMs(a) - entryEpochMs(b),
    );
    const rows = [];
    let cluster = [];
    let clusterEnd = 0;
    let clusterChannels = new Set();
    const flush = () => {
      if (!cluster.length) return;
      const wallStart = Math.min(...cluster.map(entryEpochMs));
      const wallEnd = Math.max(...cluster.map(entryEndMs));
      // Per-slot audio timeline (audio.wav) — silence-padded so it's
      // aligned with wall clock. Master playback seeks here because
      // audio.wav is always populated (even for vox slots set to
      // capture-only, i.e. to_output=false — the silence-gated mixed
      // feed skips those and would be empty in that common case).
      const audioVals = cluster
        .map((e) => e.audio_start_ms)
        .filter((v) => v != null);
      const audioStart = audioVals.length ? Math.min(...audioVals) : null;
      const byChannel = {};
      for (const e of cluster) {
        const key = e.channel || 'Unknown';
        (byChannel[key] ??= []).push(e);
      }
      rows.push({
        kind: 'speech',
        wallStart,
        wallEnd,
        audioStart,
        entries: cluster.slice(),
        byChannel,
      });
      cluster = [];
      clusterChannels = new Set();
      clusterEnd = 0;
    };
    for (const e of sorted) {
      const eStart = entryEpochMs(e);
      const key = e.channel || 'Unknown';
      // Split when: gap exceeds CLUSTER_GAP_MS, OR this channel already
      // has a clip in the current cluster (one clip per cell rule).
      if (cluster.length > 0) {
        const tooFar = eStart - clusterEnd > CLUSTER_GAP_MS;
        const dupChannel = clusterChannels.has(key);
        if (tooFar || dupChannel) {
          const prev = cluster[cluster.length - 1];
          const prevAudioEnd = (prev.audio_start_ms ?? 0) + (prev.audio_duration_ms ?? 0);
          const prevEnd = clusterEnd;
          flush();
          if (tooFar && eStart - prevEnd >= SILENCE_ROW_MIN_MS) {
            rows.push({
              kind: 'silence',
              wallStart: prevEnd,
              wallEnd: eStart,
              audioStart: prevAudioEnd,
              entries: [],
              byChannel: {},
            });
          }
        }
      }
      cluster.push(e);
      clusterChannels.add(key);
      clusterEnd = Math.max(clusterEnd, entryEndMs(e));
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
  // Duration mirrors of the two audio elements. Reading `.duration` off
  // the DOM directly isn't reactive, so a template value that depends
  // on it stays stale until *some other* reactive dep changes. Copying
  // duration into `$state` via the audio element's own events makes
  // downstream computations (Stop button swipe) update the instant the
  // metadata for a freshly-loaded track becomes known.
  let masterDurationMs = $state(0);
  let clipDurationMs = $state(0);
  const refreshMasterDuration = () => {
    const d = masterAudioEl?.duration;
    masterDurationMs = Number.isFinite(d) ? d * 1000 : 0;
  };
  const refreshClipDuration = () => {
    const d = clipAudioEl?.duration;
    clipDurationMs = Number.isFinite(d) ? d * 1000 : 0;
  };
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

  // Cap on how long a stretch of silence is allowed to play during
  // master playback before the playhead auto-advances to the next
  // speech row. Prevents the "Play All" flow from sitting through a
  // long dead-air section captured between two clusters of speech.
  const MAX_MASTER_SILENCE_MS = 5000;

  function tickPlayhead() {
    const el = playbackMode === 'clip' ? clipAudioEl : masterAudioEl;
    if (el && !el.paused) {
      playingTimeMs = el.currentTime * 1000;
      if (playbackMode === 'master') maybeSilenceSkip();
    }
    rafId = requestAnimationFrame(tickPlayhead);
  }

  /// Locate whichever recording the master player is currently reading
  /// from. Only used for the silence-skip lookup; returns null if the
  /// id doesn't match anything on the state snapshot.
  function currentPlayingRecording() {
    if (!state || playingRecordingId == null) return null;
    if (state.activeRecording?.id === playingRecordingId) return state.activeRecording;
    return state.recordings?.find((r) => r.id === playingRecordingId) ?? null;
  }

  /// Resolve a recording by id from the current state snapshot,
  /// checking both the in-flight active bucket and the saved list.
  function findRecording(id) {
    if (!state || id == null) return null;
    if (state.activeRecording?.id === id) return state.activeRecording;
    return state.recordings?.find((r) => r.id === id) ?? null;
  }

  /// Audio-timeline offset of the first transcribed utterance in a
  /// recording, or 0 if none yet. Used by "Play All" so playback skips
  /// any leading silence / untranscribed audio and starts on the first
  /// clip the user can actually read along with.
  function firstTranscribedAudioStart(rec) {
    if (!rec) return 0;
    const rows = computeRows(rec.entries);
    for (const row of rows) {
      if (row.kind === 'speech' && row.audioStart != null) return row.audioStart;
    }
    return 0;
  }

  /// Playback progress (0..1) for the Stop button's swipe fill. In
  /// master mode this is elapsed / total on the mixed-track element;
  /// in clip mode it's elapsed / clip duration. Both `playingTimeMs`
  /// and the duration mirrors are `$state`, so the swipe updates
  /// reactively the instant metadata arrives on a first-play load.
  function stopBtnProgress() {
    const durMs = playbackMode === 'clip' ? clipDurationMs : masterDurationMs;
    if (!durMs) return 0;
    return Math.max(0, Math.min(1, playingTimeMs / durMs));
  }

  /// If the master playhead has been sitting in a silence gap between
  /// two speech rows for longer than [`MAX_MASTER_SILENCE_MS`], seek
  /// straight to the next speech row's `audioStart`. `computeRows` is
  /// cheap for our data volumes so we just recompute per rAF; if the
  /// row list grows very large this can be memoized behind a $derived.
  function maybeSilenceSkip() {
    if (!masterAudioEl) return;
    const rec = currentPlayingRecording();
    if (!rec) return;
    const rows = computeRows(rec.entries).filter(
      (r) => r.kind === 'speech' && r.audioStart != null,
    );
    if (rows.length < 2) return;
    // Find the last speech row whose start is at or before the playhead.
    let curIdx = -1;
    for (let i = 0; i < rows.length; i++) {
      if (rows[i].audioStart <= playingTimeMs) curIdx = i;
      else break;
    }
    if (curIdx < 0 || curIdx >= rows.length - 1) return;
    const cur = rows[curIdx];
    const next = rows[curIdx + 1];
    // audio.wav is wall-clock aligned, so the current row's audio
    // range covers exactly its wall-clock duration.
    const curEndMs = cur.audioStart + (cur.wallEnd - cur.wallStart);
    if (
      playingTimeMs > curEndMs + MAX_MASTER_SILENCE_MS &&
      playingTimeMs < next.audioStart
    ) {
      try { masterAudioEl.currentTime = next.audioStart / 1000; } catch {}
      playingTimeMs = next.audioStart;
    }
  }

  function ensureRaf() {
    if (!rafId) rafId = requestAnimationFrame(tickPlayhead);
  }

  async function playMasterAtMs(recordingId, startMs) {
    await tick();
    if (!masterAudioEl) return;
    // Use the per-slot recording (audio.wav). It's the source of
    // truth for "everything captured this session" — every entry's
    // audio_start_ms is a position within it, so the row's audioStart
    // seeks correctly regardless of whether the vox slots were routed
    // to the mic feed.
    const wantSrc = api.recordingAudioUrl(recordingId);
    if (!masterAudioEl.src.endsWith(wantSrc)) {
      masterAudioEl.src = wantSrc;
      masterAudioEl.load();
    }
    if (clipAudioEl && !clipAudioEl.paused) clipAudioEl.pause();
    playbackMode = 'master';
    playingClipId = null;
    playingRecordingId = recordingId;
    playingTimeMs = startMs;
    // On a fresh load, `loadedmetadata` fires at readyState=1 (HAVE_METADATA)
    // — the browser knows the duration but hasn't buffered any audio
    // data yet. Setting `currentTime` at that moment schedules a seek
    // but `play()` fired in the same tick can start before the seek
    // has actually landed, so the first Play All would start from 0
    // instead of the requested `startMs`. Wait for the `seeked` event
    // to fire before starting playback so `startMs` always sticks.
    const startPlay = () => {
      masterAudioEl.play().catch((e) => console.warn('master play failed', e));
    };
    const seekAndPlay = () => {
      const targetSec = Math.max(0, startMs) / 1000;
      // Seek is a no-op when we're already there — skip the wait for
      // 'seeked' (it wouldn't fire) and just play.
      if (Math.abs(masterAudioEl.currentTime - targetSec) < 0.05) {
        startPlay();
        return;
      }
      const onSeeked = () => {
        masterAudioEl.removeEventListener('seeked', onSeeked);
        startPlay();
      };
      masterAudioEl.addEventListener('seeked', onSeeked);
      try {
        masterAudioEl.currentTime = targetSec;
      } catch {
        masterAudioEl.removeEventListener('seeked', onSeeked);
        startPlay();
      }
    };
    if (masterAudioEl.readyState >= 1) {
      seekAndPlay();
    } else {
      masterAudioEl.addEventListener('loadedmetadata', seekAndPlay, { once: true });
    }
    ensureRaf();
  }

  /// Master-column click for a row. Speech rows seek to their own
  /// `audioStart`; silence rows jump to the next speech row's
  /// `audioStart` so the user can use the master column as a
  /// scrub-forward affordance without waiting through dead air.
  function playMasterFromRow(row, rows, recordingId) {
    if (row.kind === 'speech') {
      if (row.audioStart != null) playMasterAtMs(recordingId, row.audioStart);
      return;
    }
    // Silence row: find the next speech row and jump to its audioStart.
    const idx = rows.indexOf(row);
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].audioStart != null) {
        playMasterAtMs(recordingId, rows[i].audioStart);
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
    // Completed (saved) recordings: also copy the transcript text so
    // clicking a clip serves as one-click preview + grab. Skipped for
    // the in-flight active recording where the user is usually still
    // capturing rather than harvesting text.
    const isSaved =
      recordingId != null && state?.activeRecording?.id !== recordingId;
    if (isSaved) {
      void copyEntryText(entry);
    }
  }

  function stopPlayback() {
    if (masterAudioEl && !masterAudioEl.paused) masterAudioEl.pause();
    if (clipAudioEl && !clipAudioEl.paused) clipAudioEl.pause();
    playingRecordingId = null;
    playingClipId = null;
    playingTimeMs = 0;
    playbackMode = null;
  }

  // Which entry the log should currently be scrolled to — for clip
  // playback it's the picked clip; for master playback it's whichever
  // entry the master playhead is inside right now (recomputed as the
  // playhead crosses entry boundaries). Null when nothing is playing.
  const currentPlayingEntryId = $derived.by(() => {
    if (playbackMode === 'clip') return playingClipId;
    if (playbackMode === 'master') {
      const rec = currentPlayingRecording();
      if (!rec) return null;
      for (const e of rec.entries ?? []) {
        if (e.audio_start_ms == null || e.audio_duration_ms == null) continue;
        const start = e.audio_start_ms;
        const end = start + e.audio_duration_ms;
        if (playingTimeMs >= start && playingTimeMs < end) return e.id;
      }
    }
    return null;
  });

  // Scroll the currently-playing clip into view when it starts. `nearest`
  // means we only move the page when the clip is actually off-screen —
  // if it's already visible (typical during continuous master playback
  // through consecutive rows), this is a no-op.
  $effect(() => {
    const id = currentPlayingEntryId;
    if (id == null) return;
    // Wait a frame so the just-mounted `.playing` class + any layout
    // change (sticky pane-head, etc.) settles before we measure.
    requestAnimationFrame(() => {
      const el = document.querySelector(`[data-entry-id="${id}"]`);
      if (el) el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    });
  });

  // ---- Progress predicates for the swipe animation ----

  /// An entry's clip button is "playing" when its audio range covers
  /// the current playhead — true for both explicit clip playback
  /// (`playbackMode === 'clip'`) and continuous master playback
  /// (`playbackMode === 'master'`), where the master playhead can
  /// naturally pass through many entries in sequence. Entries without
  /// an audio range (e.g. TTS) can't be located on the master timeline
  /// and are only "playing" during their own explicit clip playback.
  function isClipPlayingNow(entry) {
    if (playbackMode === 'clip') return playingClipId === entry.id;
    if (playbackMode === 'master') {
      if (entry.audio_start_ms == null || entry.audio_duration_ms == null) return false;
      const start = entry.audio_start_ms;
      const end = start + entry.audio_duration_ms;
      return playingTimeMs >= start && playingTimeMs < end;
    }
    return false;
  }
  function clipProgress(entry) {
    const dur = entry.audio_duration_ms;
    if (!dur) return 0;
    if (playbackMode === 'clip' && playingClipId === entry.id) {
      // Clip mode: playingTimeMs is the clip element's currentTime,
      // starting at 0 for the segment.
      return Math.max(0, Math.min(1, playingTimeMs / dur));
    }
    if (
      playbackMode === 'master' &&
      entry.audio_start_ms != null &&
      isClipPlayingNow(entry)
    ) {
      // Master mode: playingTimeMs is a position on the per-slot audio
      // timeline, so the fill maps to how far the master playhead has
      // travelled into this specific entry's range.
      const elapsed = playingTimeMs - entry.audio_start_ms;
      return Math.max(0, Math.min(1, elapsed / dur));
    }
    return 0;
  }

  /// A speech row is "master-playing" when the per-slot audio playhead
  /// is within `[row.audioStart, next-speech-row.audioStart)`. Silence
  /// rows are never "playing" because their master ▶ scrubs forward
  /// rather than playing anything itself; during continuous master
  /// playback the visual jumps directly from one speech row to the
  /// next as `currentTime` crosses each boundary.
  function isRowMasterPlaying(row, rows) {
    if (playbackMode !== 'master' || row.kind !== 'speech') return false;
    if (row.audioStart == null) return false;
    const idx = rows.indexOf(row);
    let end = Infinity;
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].audioStart != null) {
        end = rows[i].audioStart;
        break;
      }
    }
    return playingTimeMs >= row.audioStart && playingTimeMs < end;
  }
  /// Silence rows are "master-playing" while the playhead sits in the
  /// gap between the previous speech row's audio end and the next
  /// speech row's audio start — the same window `maybeSilenceSkip`
  /// watches. Returns false for anything outside master playback so
  /// silence rows are quiet by default.
  function isSilenceRowPlaying(row, rows) {
    if (playbackMode !== 'master' || row.kind !== 'silence') return false;
    const idx = rows.indexOf(row);
    const prev = prevSpeechRow(rows, idx);
    const next = nextSpeechRow(rows, idx);
    if (!prev || !next) return false;
    const prevEnd = prev.audioStart + (prev.wallEnd - prev.wallStart);
    return playingTimeMs >= prevEnd && playingTimeMs < next.audioStart;
  }
  /// Progress across a silence-row's swipe animation, normalised to
  /// [`MAX_MASTER_SILENCE_MS`] so the fill has a consistent visual
  /// meaning: "how close are we to the auto-skip cutoff". Shorter
  /// silences (< 5s) simply never reach 100% before playback moves on;
  /// longer silences fill to 100% just as the skip fires.
  function silenceRowProgress(row, rows) {
    if (!isSilenceRowPlaying(row, rows)) return 0;
    const idx = rows.indexOf(row);
    const prev = prevSpeechRow(rows, idx);
    if (!prev) return 0;
    const prevEnd = prev.audioStart + (prev.wallEnd - prev.wallStart);
    const elapsed = playingTimeMs - prevEnd;
    return Math.max(0, Math.min(1, elapsed / MAX_MASTER_SILENCE_MS));
  }
  function prevSpeechRow(rows, fromIdx) {
    for (let i = fromIdx - 1; i >= 0; i--) {
      if (rows[i].kind === 'speech' && rows[i].audioStart != null) return rows[i];
    }
    return null;
  }
  function nextSpeechRow(rows, fromIdx) {
    for (let i = fromIdx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].audioStart != null) return rows[i];
    }
    return null;
  }

  function rowMasterProgress(row, rows) {
    if (!isRowMasterPlaying(row, rows)) return 0;
    const idx = rows.indexOf(row);
    let end = null;
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].audioStart != null) {
        end = rows[i].audioStart;
        break;
      }
    }
    if (end == null) {
      // Last speech row — no successor to bound the range against.
      // Fall back to the row's own wall-clock duration as the master
      // slice length; caps progress at 1.0 once currentTime overshoots.
      const dur = Math.max(1, row.wallEnd - row.wallStart);
      const elapsed = playingTimeMs - row.audioStart;
      return Math.max(0, Math.min(1, elapsed / dur));
    }
    const elapsed = playingTimeMs - row.audioStart;
    const dur = Math.max(1, end - row.audioStart);
    return Math.max(0, Math.min(1, elapsed / dur));
  }

  function onPlaybackEnded() {
    playingRecordingId = null;
    playingTimeMs = 0;
  }

  function onPlaybackPause() {
    // No-op — kept as an event target so any future pause-specific
    // side-effect can hook in without threading a new handler.
  }

  function onPlaybackPlay() {
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

  // Live-buffer entries are click-to-copy (no editing). `copiedEntryId`
  // pulses briefly on the just-copied row so the user gets visual
  // confirmation without a modal or toast. Cleared by a timer.
  let copiedEntryId = $state(null);
  let copyClearTimer = 0;
  const COPY_FEEDBACK_MS = 900;

  async function copyEntryText(entry) {
    try {
      await navigator.clipboard.writeText(entry.text);
      copiedEntryId = entry.id;
      if (copyClearTimer) clearTimeout(copyClearTimer);
      copyClearTimer = setTimeout(() => {
        copiedEntryId = null;
        copyClearTimer = 0;
      }, COPY_FEEDBACK_MS);
    } catch (e) {
      err = `copy failed: ${e.message}`;
    }
  }

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

  // Same click-to-rename affordance for saved (already-completed)
  // recordings so users don't have to hunt for the sidebar's ✎ to fix
  // a name they only noticed was wrong after opening the recording.
  // Server-side, `renameRecording` routes saved ids to sqlite through
  // the same PATCH endpoint the active rename uses.
  let renamingSaved = $state(false);
  let savedRenameText = $state('');
  let savedRenameEl = $state(null);

  async function beginSavedRename() {
    const rec = selectedSaved;
    if (!rec) return;
    savedRenameText = rec.name;
    renamingSaved = true;
    await tick();
    savedRenameEl?.focus();
    savedRenameEl?.select();
  }

  async function commitSavedRename() {
    if (!renamingSaved) return;
    const rec = selectedSaved;
    renamingSaved = false;
    if (!rec) return;
    const name = savedRenameText.trim();
    if (!name || name === rec.name) return;
    await renameRecording(rec.id, name);
  }

  function cancelSavedRename() { renamingSaved = false; }

  function onSavedRenameKey(e) {
    if (e.key === 'Enter') { e.preventDefault(); commitSavedRename(); }
    else if (e.key === 'Escape') { e.preventDefault(); cancelSavedRename(); }
  }

  const isRecording = $derived(state?.mode === 'recording');
  const bufferPct = $derived(
    state ? Math.min(100, (state.bufferBytes / state.bufferMaxBytes) * 100) : 0,
  );

  // Active/saved recording log view. Two modes:
  //   * 'columns' — the default columns-per-channel table with a
  //     master column, silence rows, and per-row playback.
  //   * 'linear'  — a chronological list styled like the live buffer.
  //     Same play + edit affordances, but flat.
  // Persisted per-browser so the choice sticks across reloads. Applies
  // to both the in-flight recording banner AND the saved-recording
  // detail pane so the two views feel consistent.
  const RECORD_LOG_VIEW_KEY = 'rpg_vox.record.log_view';
  function loadRecordLogView() {
    try {
      const v = localStorage.getItem(RECORD_LOG_VIEW_KEY);
      return v === 'linear' ? 'linear' : 'columns';
    } catch {
      return 'columns';
    }
  }
  let recordLogView = $state(loadRecordLogView());
  $effect(() => {
    try { localStorage.setItem(RECORD_LOG_VIEW_KEY, recordLogView); } catch {}
  });

  // Sticky-header collapse: an IntersectionObserver watches a 1px
  // sentinel placed just above the live pane's header. When the user
  // scrolls the sentinel past the top of the viewport (accounting for
  // the pinned Menubar via `rootMargin`), the header enters `compact`
  // mode — smaller title, tighter padding — so the buffer meter and
  // any pinned controls stay visible without the title hogging the
  // top strip on a long transcript scroll.
  let liveHeadSentinel = $state(null);
  let liveHeadCompact = $state(false);
  $effect(() => {
    if (!liveHeadSentinel) return;
    const menuVar = getComputedStyle(document.documentElement)
      .getPropertyValue('--menubar-height')
      .trim();
    const menuPx = parseInt(menuVar, 10) || 0;
    const io = new IntersectionObserver(
      ([entry]) => {
        liveHeadCompact = !entry.isIntersecting;
      },
      { rootMargin: `-${menuPx}px 0px 0px 0px`, threshold: 0 },
    );
    io.observe(liveHeadSentinel);
    return () => io.disconnect();
  });
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
       through subsequent rows; Clip (per-entry segment) is one-shot.
       `ondurationchange` + `onloadedmetadata` mirror the browser's
       duration into `$state` so the Stop button's swipe fill reacts
       the moment metadata for a fresh track arrives. -->
  <audio
    bind:this={masterAudioEl}
    onplay={onPlaybackPlay}
    onpause={onPlaybackPause}
    onended={onPlaybackEnded}
    ondurationchange={refreshMasterDuration}
    onloadedmetadata={refreshMasterDuration}
    preload="none"
  ></audio>
  <audio
    bind:this={clipAudioEl}
    onplay={onPlaybackPlay}
    onpause={onPlaybackPause}
    onended={onPlaybackEnded}
    ondurationchange={refreshClipDuration}
    onloadedmetadata={refreshClipDuration}
    preload="none"
  ></audio>
{/snippet}

{#snippet playAllBtn(recordingId)}
  {#if playbackMode != null && playingRecordingId === recordingId}
    <button
      type="button"
      class="topbar-btn stop"
      onclick={stopPlayback}
      title="Stop playback"
    >
      <div class="stop-fill" style="width: {stopBtnProgress() * 100}%" aria-hidden="true"></div>
      <span class="stop-label">■ Stop</span>
    </button>
  {:else}
    <button
      type="button"
      class="topbar-btn"
      onclick={() => playMasterAtMs(recordingId, firstTranscribedAudioStart(findRecording(recordingId)))}
      title="Play the recording from the first transcribed clip"
    >▶ Play All</button>
  {/if}
{/snippet}

{#snippet viewToggleBtn()}
  <!-- Two-position pill toggle. Highlights the active mode; clicking
       the OTHER label flips the view. Shared between the active-
       recording banner and (potentially) the saved-recording pane so
       both places stay in sync via the same persisted `recordLogView`. -->
  <div class="view-toggle" role="group" aria-label="Log view">
    <button
      type="button"
      class="view-toggle-btn"
      class:active={recordLogView === 'columns'}
      onclick={() => (recordLogView = 'columns')}
      title="Show as columns per channel"
      aria-pressed={recordLogView === 'columns'}
    >⋮⋮</button>
    <button
      type="button"
      class="view-toggle-btn"
      class:active={recordLogView === 'linear'}
      onclick={() => (recordLogView = 'linear')}
      title="Show as a chronological list"
      aria-pressed={recordLogView === 'linear'}
    >≡</button>
  </div>
{/snippet}

{#snippet logTable(recording, canonicalChannels)}
  {@const rows = computeRows(recording.entries)}
  {@const channels = extractChannels(recording.entries, canonicalChannels)}
  {@const epoch = recordingEpochMs(recording)}
  {@const recId = recording.id}
  <div class="log-scroller">
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
                <span class="silence-tag" class:playing={isSilenceRowPlaying(row, rows)}>
                  {#if isSilenceRowPlaying(row, rows)}
                    <div class="clip-fill" style="width: {silenceRowProgress(row, rows) * 100}%" aria-hidden="true"></div>
                  {/if}
                  <span class="silence-text">silence · {Math.round((row.wallEnd - row.wallStart) / 1000)}s</span>
                </span>
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

{#snippet linearLog(recording, canonicalChannels)}
  {@const recId = recording.id}
  {@const fallback = canonicalChannels[0] ?? 'Vox 1'}
  <!-- Chronological list of every entry, styled like the ephemeral
       live buffer. Skips the master column and silence rows (those are
       only meaningful in the timeline-oriented columns view) and reuses
       `clipCell` so per-entry play + edit still work — including the
       playing/copied/provisional state classes. Labels are grouped in
       a `.row-labels` div so grid-template-areas can pin channel +
       speaker on top and the timestamp beneath, with the text spanning
       both rows on the right. -->
  <ul class="entries linear-log">
    {#each recording.entries as e (e.id)}
      {@const ch = e.channel || fallback}
      {@const isTts = ch === 'TTS'}
      <!-- Title for the avatar: voice profile for TTS (falls back to
           "TTS" for older widgets whose voice_label column is unset),
           speaker attribution (if any) for vox, else the slot name. -->
      {@const title = isTts
        ? (e.speaker || 'TTS')
        : (e.speaker || ch)}
      {@const channelType = isTts ? 'TTS' : 'VOX'}
      <li class:playing={isClipPlayingNow(e)} class:provisional={e.provisional}>
        <!-- Compact portrait avatar with everything needed to identify
             the speaker at a glance. Uniform fixed-width so the
             transcript text on the right lines up across every row.
             Avatar image is a placeholder (initial letter over a
             deterministic tint) until per-entry avatar metadata is
             wired through the recording pipeline. -->
        <div class="entry-avatar" title={fmtTime(e.created_at)}>
          <div class="avatar-portrait" style="background: {avatarTint(title)}">
            <span class="avatar-initial">{initialFrom(title)}</span>
          </div>
          <span class="avatar-type" class:tts={isTts}>{channelType}</span>
          <span class="avatar-title" title={title}>{title}</span>
          <span class="avatar-ts">{fmtTimeUtcCompact(e.created_at)}</span>
        </div>
        {@render clipCell(e, ch, recId)}
      </li>
    {/each}
  </ul>
{/snippet}

{#snippet clipCell(entry, channel, recId)}
  <div
    class="clip-btn"
    data-entry-id={entry.id}
    class:playing={isClipPlayingNow(entry)}
    class:copied={copiedEntryId === entry.id}
    class:tts={channel === 'TTS'}
    class:provisional={entry.provisional}
  >
    {#if isClipPlayingNow(entry)}
      <div class="clip-fill" style="width: {clipProgress(entry) * 100}%" aria-hidden="true"></div>
    {/if}
    {#if entry.speaker}
      <span class="speaker-tag" title="Speaker attribution">{entry.speaker}</span>
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
        title={isClipPlayingNow(entry) ? 'Click to stop · double-click to edit' : 'Click to play · double-click to edit'}
        onclick={() => isClipPlayingNow(entry) ? stopPlayback() : playClip(entry, recId)}
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
    onSelect={(k) => { selected = k; }}
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
    {:else if selected === 'live'}
      <!-- Rolling ephemeral buffer view. Sentinel is a 1px shim sitting
           just above the sticky header so IntersectionObserver can flip
           the header into its `compact` mode the moment scroll pins it
           to the top. Keeps the buffer meter visible without the title
           eating vertical space during a long transcript scroll. -->
      <div bind:this={liveHeadSentinel} class="head-sentinel"></div>
      <div class="pane-head live" class:compact={liveHeadCompact}>
        <h1>Ephemeral live transcript (buffered; not recorded)
          {#if !state.sttEnabled}
            <span class="stt-off"> · STT disabled</span>
          {/if}
        </h1>
        <div
          class="buf-meter"
          title="{fmtBytes(state.bufferBytes)} of {fmtBytes(state.bufferMaxBytes)}"
        >
          <div class="buf-fill" style="width: {bufferPct}%"></div>
        </div>
        <button
          type="button"
          class="live-rec-btn"
          disabled={busy || state.activeRecording != null}
          title={state.activeRecording != null
            ? 'Stop the current recording first'
            : 'Start a new recording'}
          aria-label="Start a new recording"
          onclick={beginNewRecording}
        >
          <span class="live-rec-dot" aria-hidden="true"></span>
          <span class="live-rec-label">Record</span>
        </button>
      </div>
      {#if state.buffer.length === 0}
        <div class="empty pane-empty">
          nothing spoken yet — the {channelFallback} channel is quiet
        </div>
      {:else}
        <ul class="entries">
          {#each state.buffer as e (e.id)}
            <li class:copied={copiedEntryId === e.id} class:provisional={e.provisional}>
              <span class="channel-tag">{e.channel || channelFallback}</span>
              {#if e.speaker}
                <span class="speaker-tag" title="Voice / profile">{e.speaker}</span>
              {/if}
              <span class="ts">{fmtTime(e.created_at)}</span>
              <button
                type="button"
                class="text text-btn"
                title={copiedEntryId === e.id ? 'Copied!' : 'Click to copy transcript'}
                onclick={() => copyEntryText(e)}
              >{e.text}</button>
              {#if copiedEntryId === e.id}
                <span class="copy-flag" aria-live="polite">copied ✓</span>
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
          {@render viewToggleBtn()}
          {@render playAllBtn(state.activeRecording.id)}
          <button type="button" class="primary" disabled={busy} onclick={stopRecording}>
            Stop &amp; Save
          </button>
        </div>
      </div>
      {@render hiddenAudios()}
      {#if state.activeRecording.entries.length === 0}
        <div class="empty pane-empty">listening…</div>
      {:else if recordLogView === 'linear'}
        {@render linearLog(state.activeRecording, state.channelNames ?? [])}
      {:else}
        {@render logTable(state.activeRecording, state.channelNames ?? [])}
      {/if}
    {:else if selectedSaved}
      <!-- Saved recording detail. Kept as a single-line top-aligned bar
           so it doesn't push the log down. Non-essential meta (created
           timestamp, utterance count) is dropped in favor of duration
           only, which is what actually matters when scrubbing playback.
           Title is click-to-rename — server routes the PATCH through
           the same endpoint the active recording uses. -->
      <div class="pane-head saved">
        {#if renamingSaved}
          <input
            class="saved-title-rename"
            type="text"
            bind:this={savedRenameEl}
            bind:value={savedRenameText}
            onkeydown={onSavedRenameKey}
            onblur={commitSavedRename}
            aria-label="Rename recording"
          />
        {:else}
          <button
            type="button"
            class="saved-title"
            title="Click to rename"
            onclick={beginSavedRename}
          >{selectedSaved.name}</button>
        {/if}
        <span class="saved-meta">{fmtDur(selectedSaved.duration_ms)}</span>
        <div class="head-actions">
          {@render playAllBtn(selectedSaved.id)}
          <a
            class="download-btn icon"
            href={api.recordingArchiveUrl(selectedSaved.id)}
            download
            title="Download a zip with transcript.json, mixed.wav, audio.wav, and per-utterance clips"
            aria-label="Download archive"
          >⤓</a>
        </div>
      </div>
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
    align-items: flex-start;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    /* Tight vertical rhythm so the log gets as many lines as
       possible on a 720p screen. Horizontal padding stays generous
       for readability. */
    padding: 4px 16px 6px;
    gap: 6px;
  }

  /* Pinned pane header so the recording title, timer, and Stop/Play
     controls stay reachable no matter how far the log has scrolled.
     `top` anchors just below the sticky Menubar. An opaque background
     is essential — otherwise log rows would visibly slide *through* the
     header as they scroll. On mobile the header carves out room for
     the fixed hamburger (34px wide + a little breathing room). */
  .pane-head {
    position: sticky;
    top: var(--menubar-height);
    z-index: 5;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    padding: 4px 4px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
    transition: padding 0.15s ease;
  }
  /* 1px shim above the sticky header — IntersectionObserver watches
     whether this element is still visible in the scroll port to detect
     when the header has stuck. Negative margin keeps it from adding
     layout height. */
  .head-sentinel {
    height: 1px;
    margin: -1px 0 0 0;
    pointer-events: none;
  }
  /* Live pane variant: the buffer meter lives INSIDE the sticky header
     (instead of floating below it in the scroll flow), so the "how full
     is the ring buffer" readout stays visible regardless of scroll. */
  .pane-head.live {
    align-items: center;
  }
  .pane-head.live .buf-meter {
    margin-left: auto;
    flex: 0 0 auto;
  }
  /* Start-recording affordance pinned to the far-right corner of the
     sticky header. Visually related to the active pane's `.rec-badge`
     (same pulsing red dot) so the color language is consistent across
     "record" and "recording in progress" states — but rendered as a
     button so it reads as clickable rather than a status indicator.
     Disabled when an active recording already exists; matches the
     sidebar `+` button's guard so both entry points behave the same. */
  .pane-head.live .live-rec-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border: 1px solid rgba(255,92,92,0.5);
    border-radius: 999px;
    background: rgba(255,92,92,0.08);
    color: #ff8a8a;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    flex: 0 0 auto;
    transition: background 0.15s ease, border-color 0.15s ease;
  }
  .pane-head.live .live-rec-btn:hover:not(:disabled) {
    background: rgba(255,92,92,0.18);
    border-color: rgba(255,92,92,0.8);
  }
  .pane-head.live .live-rec-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .live-rec-dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #ff5c5c;
    box-shadow: 0 0 6px rgba(255,92,92,0.6);
    animation: rec-pulse 1.5s ease-in-out infinite;
  }
  /* Compact-header form: drop the label so the button becomes a
     circular red dot in the corner. Preserves the affordance without
     eating horizontal space next to the meter on a scrolled page. */
  .pane-head.compact .live-rec-btn { padding: 3px 6px; }
  .pane-head.compact .live-rec-label { display: none; }
  /* Compact form — activated once the header is pinned to the top. Just
     enough shrink to reclaim ~half the vertical footprint without
     ellipsizing the title. The h1 stays legible; only the padding and
     font-size dial back. */
  .pane-head.compact {
    padding-top: 2px;
    padding-bottom: 2px;
  }
  .pane-head.compact h1 { font-size: 12px; }
  @media (max-width: 1079px) {
    .pane-head { padding-left: 44px; }
  }
  .pane-head h1 { font-size: 16px; margin: 0; line-height: 1.2; }

  /* Saved-recording variant: single top-aligned row, title truncates
     with ellipsis before wrapping, minimal meta (duration only), and
     the action buttons stay pinned to the right at a fixed size so the
     header doesn't grow past one line. */
  .pane-head.saved {
    align-items: center;
    flex-wrap: nowrap;
    padding: 6px 4px;
  }
  /* Same 44px left inset as the other pane-head variants get in the
     shared mobile rule — the `padding` shorthand above wipes it out on
     the saved variant, so reinstate it here so the title clears the
     fixed hamburger instead of hiding under it. */
  @media (max-width: 1079px) {
    .pane-head.saved { padding-left: 44px; }
  }
  .pane-head.saved .saved-title {
    font-size: 15px;
    font-weight: 600;
    margin: 0;
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    /* Reset the base button styling so this reads as a title, not a
       CTA. Hover reveals the rename affordance. */
    background: transparent;
    color: var(--text);
    border: 1px dashed transparent;
    padding: 2px 6px;
    text-align: left;
    cursor: pointer;
    font-family: inherit;
    line-height: 1.2;
  }
  .pane-head.saved .saved-title:hover {
    border-color: var(--border);
    background: rgba(255,255,255,0.03);
  }
  .pane-head.saved .saved-title-rename {
    flex: 1 1 auto;
    min-width: 0;
    font-size: 15px;
    font-weight: 600;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 2px 6px;
    line-height: 1.2;
    font-family: inherit;
  }
  .pane-head.saved .saved-meta {
    color: var(--muted);
    font-size: 12px;
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
    white-space: nowrap;
  }
  .pane-head.saved .head-actions { flex-shrink: 0; }
  /* Icon-only download variant — the full "Download archive" label
     bloats the single-line header; keep the affordance as a compact
     glyph with the descriptive text moved to `title=`. */
  .download-btn.icon {
    padding: 4px 8px;
    font-size: 14px;
  }
  .stt-off { color: var(--err); font-weight: 500; font-size: 12px; }

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

  .pane-head.active {
    padding: 10px 14px;
    /* Layer the red REC tint over an opaque bg so this variant is also
       fully opaque while still reading tinted (rgba alone would show
       scrolling log content through the sticky header). */
    background:
      linear-gradient(rgba(255, 80, 80, 0.06), rgba(255, 80, 80, 0.06)),
      var(--bg);
    border: 1px solid rgba(255, 80, 80, 0.4);
    border-radius: 10px;
  }
  @media (max-width: 1079px) {
    .pane-head.active { padding-left: 44px; }
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

  /* Segmented toggle for switching the recording log between the
     columns-per-channel table and a chronological linear list. Lives
     in the active-actions strip alongside Play All / Stop so both
     entry points to the log's presentation live in the same header. */
  .view-toggle {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
    align-self: center;
  }
  .view-toggle-btn {
    background: transparent;
    color: var(--muted);
    border: none;
    padding: 4px 10px;
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    min-width: 32px;
  }
  .view-toggle-btn:hover:not(.active) {
    background: rgba(255,255,255,0.04);
    color: var(--text);
  }
  .view-toggle-btn.active {
    background: rgba(122,162,255,0.2);
    color: var(--accent);
  }
  .view-toggle-btn + .view-toggle-btn { border-left: 1px solid var(--border); }

  /* Linear-log flavor of the entry list. Uniform portrait avatar box
     on the left carries the type + title + timestamp, transcript text
     fills the rest. The fixed avatar width is what makes text on
     every row line up flush at the same left edge. */
  ul.entries.linear-log li {
    grid-template-columns: var(--linear-avatar-w) 1fr;
    grid-template-rows: auto;
    align-items: stretch;
    gap: 8px;
    padding: 2px 4px;
  }
  ul.entries.linear-log {
    /* Single knob for the whole log's avatar width so a tweak keeps
       every row aligned. Sized to fit "· shout"-length profile names
       without wrapping mid-word too aggressively. */
    --linear-avatar-w: 72px;
  }
  .entry-avatar {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1px;
    padding: 2px 3px;
    background: rgba(0, 0, 0, 0.28);
    border: 1px solid rgba(255, 255, 255, 0.05);
    border-radius: 5px;
    text-align: center;
    overflow: hidden;
    min-width: 0;
  }
  .avatar-portrait {
    width: 32px;
    height: 32px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: rgba(255, 255, 255, 0.9);
    font-size: 15px;
    font-weight: 600;
    line-height: 1;
    /* Subtle inner ring so the portrait reads as a distinct chip
       against the row's own dark background. */
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.08);
    user-select: none;
  }
  .avatar-initial {
    letter-spacing: 0;
  }
  .avatar-type {
    font-size: 8px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--accent);
    line-height: 1;
  }
  .avatar-type.tts { color: #ffcc66; }
  .avatar-title {
    font-size: 9px;
    line-height: 1.15;
    color: var(--text);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 0 2px;
  }
  .avatar-ts {
    font-size: 8px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  ul.entries.linear-log .clip-btn {
    /* The avatar is column 1; the text belongs in the 1fr second
       column. Explicit pin (rather than relying on auto-place) so a
       future addition of any extra child never shoves the text out
       from under the transcript column. */
    grid-column: 2;
    align-self: center;
    background: transparent;
    border-color: transparent;
    padding: 2px 4px;
    margin-bottom: 0;
    min-width: 0;
  }
  ul.entries.linear-log li.playing .clip-btn {
    border-color: rgba(122,162,255,0.6);
    box-shadow: 0 0 0 1px rgba(122,162,255,0.35);
    background: rgba(0,0,0,0.2);
  }
  /* `clipCell` renders its own inline speaker-tag inside the clip-btn
     for the columns view. In the linear log we surface it up-front in
     `.row-labels`, so hide the duplicate to avoid showing the voice
     name twice per row. */
  ul.entries.linear-log .clip-btn > .speaker-tag { display: none; }
  /* Tighter line-height inside the linear log so multi-line transcript
     text doesn't waste vertical space between wrapped lines — the row
     height is already dominated by the avatar box, no reason for the
     text to add breathing room on top of that. */
  ul.entries.linear-log .clip-text {
    line-height: 1.25;
  }
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
  .head-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }
  /* Play All / Stop button — lives in the pane top bar so its
     presence doesn't shift the log table around when playback starts. */
  .topbar-btn {
    background: rgba(122,162,255,0.12);
    color: var(--text);
    border: 1px solid rgba(122,162,255,0.4);
    padding: 4px 12px;
    font-size: 12px;
    font-weight: 600;
    border-radius: 4px;
    cursor: pointer;
    white-space: nowrap;
  }
  .topbar-btn:hover { background: rgba(122,162,255,0.22); }
  .topbar-btn.stop {
    background: rgba(255,92,92,0.14);
    color: #ffbdbd;
    border-color: rgba(255,92,92,0.5);
    /* Establish a positioning + clipping context for the fill overlay
       so it can sweep across the button width without leaking past the
       border-radius. */
    position: relative;
    overflow: hidden;
  }
  .topbar-btn.stop:hover { background: rgba(255,92,92,0.24); }
  /* Swipe overlay showing playback progress (0..1 of the full track
     when master mode, or the current clip in clip mode). Same shape as
     `.clip-fill` but red-tinted to match the Stop button. */
  .stop-fill {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    background: linear-gradient(
      to right,
      rgba(255, 92, 92, 0.28),
      rgba(255, 92, 92, 0.5)
    );
    border-right: 1px solid rgba(255, 92, 92, 0.85);
    pointer-events: none;
    z-index: 0;
    /* Small transition smooths out the per-frame width updates so the
       swipe reads as a continuous sweep even though we're stepping
       from rAF ticks. */
    transition: width 60ms linear;
  }
  .stop-label {
    position: relative;
    z-index: 1;
  }

  ul.entries {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  ul.entries li {
    display: grid;
    grid-template-columns: max-content max-content 1fr;
    grid-template-rows: auto auto;
    gap: 2px 8px;
    padding: 3px 6px;
    border-radius: 4px;
    background: rgba(0,0,0,0.15);
    align-items: baseline;
    position: relative;
    overflow: hidden;
    /* Column-flex parents default children to flex-shrink: 1. Without
       this, once the buffer exceeds the pane height each row squeezes
       and its text is clipped by the overflow: hidden above, instead
       of the list scrolling. */
    flex-shrink: 0;
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
  /* Transient "copied ✓" pulse on the just-clicked live-buffer row. The
     copy-flag chip fades in beside the text, and the whole LI briefly
     lights up so the click lands unambiguously. */
  ul.entries li.copied { background: rgba(46, 204, 74, 0.14); }
  ul.entries li.provisional { opacity: 0.7; }
  ul.entries li.provisional .text { font-style: italic; }
  .copy-flag {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: #b7f0c4;
    background: rgba(46, 204, 74, 0.18);
    padding: 2px 6px;
    border-radius: 3px;
    white-space: nowrap;
    align-self: center;
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

  /* ---- Log table (columns-per-channel view) ---- */
  .log-scroller {
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
    background: var(--panel);
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
    /* Positioned so the .clip-fill overlay (below) can sit behind the
       text and sweep left→right during master playback. */
    position: relative;
    overflow: hidden;
  }
  .silence-tag.playing {
    color: var(--text);
    background: rgba(122,162,255,0.14);
  }
  .silence-tag .clip-fill {
    /* Reuse the same swipe visual as clip-btn / master-btn so the
       animation reads as "same kind of playhead". */
    z-index: 0;
  }
  .silence-tag .silence-text {
    position: relative;
    z-index: 1;
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
  /* Brief green outline pulse on saved-recording clips whose text was
     just copied to the clipboard. Uses a distinct color from
     `.playing` so both cues can co-exist when a click both plays and
     copies. */
  .clip-btn.copied {
    border-color: rgba(46, 204, 74, 0.55);
    box-shadow: 0 0 0 1px rgba(46, 204, 74, 0.25);
  }
  .clip-btn.tts { background: rgba(255,204,102,0.06); }
  /* Streaming-STT partials: italic + dimmed so a mid-utterance
     hypothesis is visually distinct from the polished final. The
     final decode replaces the entry in place (same id) and clears
     this class in the next poll. */
  .clip-btn.provisional { opacity: 0.72; background: rgba(80,140,220,0.05); }
  .clip-btn.provisional .clip-text { font-style: italic; }
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
