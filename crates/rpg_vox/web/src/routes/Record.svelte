<script>
  import { onMount, onDestroy, tick } from 'svelte';
  import * as api from '../lib/api.js';
  import RecordingsSidebar from '../components/RecordingsSidebar.svelte';

  // Server-side state, refreshed every POLL_MS. Structure:
  //   { mode: 'idle' | 'recording',
  //     paragraphsByChannel: [{ channel, paragraphs: Paragraph[] }],
  //     activeRecording: { id, name, paragraphs, created_at, duration_ms,
  //                        mixed_duration_ms } | null,
  //     recordings: [{ id, name, paragraphs, duration_ms, sample_rate,
  //                    audio_url, created_at }],
  //     channelNames: string[],  // display names per Vox slot (canonical
  //                              //   channel order)
  //     sttEnabled: bool, sampleRate: number }
  //
  // Paragraph = { id, channel, speaker?, hardened, text, raw_text,
  //               clips: ClipRef[], start_wall_ms, end_wall_ms, created_at }
  // ClipRef   = { id, audio_start_ms?, audio_duration_ms?, start_wall_ms,
  //               text, provisional, audio_url?, mixed_start_ms? }
  let state = $state(null);
  let err = $state(null);
  let busy = $state(false);

  // "Reinterpret with LLM" toggle for the live pane. Off by default.
  // The value is authoritative on the client (localStorage) — the
  // server just mirrors last-writer. onMount reads localStorage and
  // POSTs once so a fresh server picks up the operator's preference;
  // every checkbox change POSTs the new value. During a named
  // recording the server ignores this flag and always runs the LLM.
  const LLM_WHEN_IDLE_KEY = 'rpgvox.llmWhenIdle';
  let llmWhenIdle = $state(false);
  async function onLlmWhenIdleChange() {
    try {
      localStorage.setItem(LLM_WHEN_IDLE_KEY, llmWhenIdle ? '1' : '0');
      await api.setLlmWhenIdle(llmWhenIdle);
    } catch (e) {
      console.warn('llm-when-idle put failed', e);
    }
  }

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
    // Restore the "Reinterpret with LLM" toggle from localStorage
    // before the first poll so the checkbox reflects the persisted
    // choice, and POST it once so the server (which just restarted
    // and defaults to off) picks it up. Failures are non-fatal —
    // worst case the server stays at its default until the user
    // clicks the box.
    try {
      const stored = localStorage.getItem(LLM_WHEN_IDLE_KEY);
      llmWhenIdle = stored === '1' || stored === 'true';
      await api.setLlmWhenIdle(llmWhenIdle);
    } catch (e) {
      // Non-fatal — the checkbox still works; server just misses
      // the initial sync.
      console.warn('llm-when-idle init failed', e);
    }
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
  // Wall-clock ms → local HH:MM:SS. Used inside a paragraph header so
  // the moment the paragraph started reading is at a glance.
  function fmtWallTime(ms) {
    if (!ms) return '';
    const d = new Date(ms);
    return d.toLocaleTimeString([], {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
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

  // Compact "M:SS" for an elapsed millisecond count. Used to label each
  // clip chip with its offset from the parent paragraph's start.
  function fmtRelMs(ms) {
    const rel = Math.max(0, Math.floor((ms ?? 0) / 1000));
    const m = Math.floor(rel / 60);
    const s = rel % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
  }

  // ---- Paragraph clustering ----
  //
  // Sort paragraphs by wall-clock start, then sweep: any paragraph
  // whose start falls at or before the current cluster's running end
  // is absorbed into the cluster (row spans multiple channels).
  // Otherwise the cluster flushes as a speech row, an optional silence
  // row is inserted if the gap ≥ SILENCE_ROW_MIN_MS, and the paragraph
  // seeds the next cluster.
  //
  // Row shape:
  //   {
  //     kind: 'speech' | 'silence',
  //     wallStart, wallEnd,
  //     audioStart: number | null,   // min clip.audio_start_ms across
  //                                  //   the cluster (null for TTS-only
  //                                  //   or missing audio_start_ms)
  //     byChannel: Record<string, Paragraph[]>,
  //     channels: string[]           // canonical-ordered, TTS at end
  //   }
  const SILENCE_ROW_MIN_MS = 3000;

  // Union of channel names in the paragraph set, intersected with the
  // canonical list (from state.channelNames) so we never emit an empty
  // column. `TTS` moves to the end so vox reads left, synth right.
  function orderChannels(clusterChannelSet, canonical = []) {
    const cols = [];
    for (const c of canonical) {
      if (clusterChannelSet.has(c)) cols.push(c);
    }
    for (const c of clusterChannelSet) {
      if (!cols.includes(c)) cols.push(c);
    }
    const tts = cols.indexOf('TTS');
    if (tts >= 0) {
      cols.splice(tts, 1);
      cols.push('TTS');
    }
    return cols;
  }

  // Precompute per-paragraph audio range on the paragraph object itself
  // so the rAF loop's O(1) lookup stays O(1). `audioStart`/`audioEnd`
  // are keyed on the per-slot audio.wav timeline (via clip.audio_start_ms
  // + audio_duration_ms). Paragraphs with no positioned clip (TTS-only)
  // get `audioStart = null` and won't be highlighted during master
  // playback.
  function paragraphAudioRange(p) {
    let start = null;
    let end = null;
    for (const c of p.clips ?? []) {
      const s = c.audio_start_ms;
      const d = c.audio_duration_ms;
      if (s == null) continue;
      if (start == null || s < start) start = s;
      const e = s + (d ?? 0);
      if (end == null || e > end) end = e;
    }
    return { audioStart: start, audioEnd: end };
  }

  function computeClusters(paragraphs, canonical = []) {
    const sorted = [...(paragraphs ?? [])].sort(
      (a, b) => (a.start_wall_ms ?? 0) - (b.start_wall_ms ?? 0),
    );
    const rows = [];
    let cluster = [];
    let clusterEnd = 0;
    const flush = () => {
      if (!cluster.length) return;
      const byChannel = {};
      const chSet = new Set();
      let audioStart = null;
      let wallStart = Infinity;
      let wallEnd = 0;
      for (const p of cluster) {
        const key = p.channel || 'Unknown';
        (byChannel[key] ??= []).push(p);
        chSet.add(key);
        if ((p.start_wall_ms ?? 0) < wallStart) wallStart = p.start_wall_ms ?? 0;
        if ((p.end_wall_ms ?? 0) > wallEnd) wallEnd = p.end_wall_ms ?? 0;
        const range = paragraphAudioRange(p);
        if (range.audioStart != null) {
          if (audioStart == null || range.audioStart < audioStart) {
            audioStart = range.audioStart;
          }
        }
      }
      rows.push({
        kind: 'speech',
        wallStart,
        wallEnd,
        audioStart,
        byChannel,
        channels: orderChannels(chSet, canonical),
      });
      cluster = [];
      clusterEnd = 0;
    };
    for (const p of sorted) {
      const pStart = p.start_wall_ms ?? 0;
      const pEnd = p.end_wall_ms ?? pStart;
      if (cluster.length === 0) {
        cluster.push(p);
        clusterEnd = pEnd;
        continue;
      }
      if (pStart <= clusterEnd) {
        // Overlaps → same cluster row.
        cluster.push(p);
        if (pEnd > clusterEnd) clusterEnd = pEnd;
        continue;
      }
      // Gap: flush current cluster; conditionally emit silence.
      const gap = pStart - clusterEnd;
      const prevEnd = clusterEnd;
      // audio-timeline end of the just-closed cluster's last paragraph,
      // if any of the cluster's paragraphs had audio positioning. Used
      // as the silence row's audio anchor so a click on the silence row
      // can jump the master playhead across the gap.
      let prevAudioEnd = null;
      for (const p2 of cluster) {
        const range = paragraphAudioRange(p2);
        if (range.audioEnd != null) {
          if (prevAudioEnd == null || range.audioEnd > prevAudioEnd) {
            prevAudioEnd = range.audioEnd;
          }
        }
      }
      flush();
      if (gap >= SILENCE_ROW_MIN_MS) {
        rows.push({
          kind: 'silence',
          wallStart: prevEnd,
          wallEnd: pStart,
          audioStart: prevAudioEnd,
          byChannel: {},
          channels: [],
        });
      }
      cluster.push(p);
      clusterEnd = pEnd;
    }
    flush();
    return rows;
  }

  // Chronological playback state. One shared <audio> element per pane
  // (active + saved) is used so seeking + play is atomic. We track
  // currentTime with a rAF loop so the "playing now" highlight moves
  // smoothly; server-side gets pinged with /record/playback so any
  // VAD-captured vox during playback is flagged (see push_transcript).
  let playingRecordingId = $state(null); // which recording is currently sounding
  let playingTimeMs = $state(0);          // ms — master timeline in 'master' mode, clip offset in 'clip' mode
  let masterAudioEl = $state(null);       // hidden <audio> for the silence-gated mixed track
  let clipAudioEl = $state(null);         // hidden <audio> for one-shot per-clip segment playback
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
  // 'clip' when a single per-channel clip is playing (no advance).
  // null when nothing is playing. Drives which element the rAF loop
  // samples and which row/clip button lights up.
  let playbackMode = $state(null);
  // Which clip id is playing under 'clip' mode. Only set when
  // playbackMode === 'clip'.
  let playingClipId = $state(null);
  let rafId = 0;

  // ---- Master vs clip playback ----
  //
  // Master: click a row's ▶. Seeks the (hidden) per-slot audio element
  // to that row's `audioStart` and lets it play straight through —
  // subsequent rows highlight naturally as the playhead crosses their
  // `audioStart` boundaries. Silence rows have no play affordance of
  // their own; continuous master playback either falls through them or
  // triggers the silence-skip cutoff.
  //
  // Clip: click a per-paragraph clip chip. Fetches just that clip's
  // audio segment via /record/recordings/:id/segment and plays it in a
  // second hidden element. No auto-advance — one segment, done.

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
  /// paragraph the user can actually read along with.
  function firstTranscribedAudioStart(rec) {
    if (!rec) return 0;
    for (const p of rec.paragraphs ?? []) {
      for (const c of p.clips ?? []) {
        if (c.audio_start_ms != null) return c.audio_start_ms;
      }
    }
    return 0;
  }

  /// Playback progress (0..1) for the Stop button's swipe fill. In
  /// master mode this is elapsed / total on the per-slot audio element;
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
  /// straight to the next speech row's `audioStart`.
  function maybeSilenceSkip() {
    if (!masterAudioEl) return;
    const rec = currentPlayingRecording();
    if (!rec) return;
    const rows = computeClusters(rec.paragraphs, state?.channelNames ?? []).filter(
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
    // Use the per-slot recording (audio.wav). It's the source of truth
    // for "everything captured this session" — every clip's
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

  /// One-shot playback of a single clip's audio. Accepts a partial
  /// clip descriptor so callers can pass an isolated `{ id, audio_url }`
  /// (for TTS widgets) or a `{ id, audio_start_ms, audio_duration_ms }`
  /// pair (for saved-recording vox segments).
  ///
  /// **Risk 1 (live-pane vox):** the segment endpoint only exists on
  /// `/record/recordings/:id/segment` — there is no per-slot ring
  /// endpoint. Live-pane vox clips (recId == null && !audio_url) simply
  /// have no server-serveable audio and the chip is disabled at the
  /// callsite.
  async function playClip(clipLike, recordingId) {
    await tick();
    if (!clipAudioEl) return;
    let src;
    if (clipLike.audio_url) {
      src = clipLike.audio_url;
    } else if (
      recordingId != null &&
      clipLike.audio_start_ms != null &&
      clipLike.audio_duration_ms != null
    ) {
      src = api.recordingSegmentUrl(
        recordingId,
        clipLike.audio_start_ms,
        clipLike.audio_duration_ms,
      );
    } else {
      return;
    }
    clipAudioEl.src = src;
    clipAudioEl.load();
    if (masterAudioEl && !masterAudioEl.paused) masterAudioEl.pause();
    playbackMode = 'clip';
    playingClipId = clipLike.id;
    playingRecordingId = recordingId;
    playingTimeMs = 0;
    clipAudioEl.play().catch((e) => console.warn('clip play failed', e));
    ensureRaf();
  }

  function stopPlayback() {
    if (masterAudioEl && !masterAudioEl.paused) masterAudioEl.pause();
    if (clipAudioEl && !clipAudioEl.paused) clipAudioEl.pause();
    playingRecordingId = null;
    playingClipId = null;
    playingTimeMs = 0;
    playbackMode = null;
  }

  // Which paragraph the log should currently highlight — for clip
  // playback it's the paragraph owning the clip; for master playback
  // it's whichever paragraph's [audioStart, audioEnd] contains the
  // master playhead, falling back to nearest previous.
  const currentPlayingParagraphId = $derived.by(() => {
    if (playbackMode === 'clip') {
      const rec = currentPlayingRecording();
      if (!rec) return null;
      for (const p of rec.paragraphs ?? []) {
        for (const c of p.clips ?? []) {
          if (c.id === playingClipId) return p.id;
        }
      }
      return null;
    }
    if (playbackMode === 'master') {
      const rec = currentPlayingRecording();
      if (!rec) return null;
      let nearest = null;
      let nearestStart = -Infinity;
      for (const p of rec.paragraphs ?? []) {
        const { audioStart, audioEnd } = paragraphAudioRange(p);
        if (audioStart == null) continue;
        if (playingTimeMs >= audioStart && (audioEnd == null || playingTimeMs < audioEnd)) {
          return p.id;
        }
        if (audioStart <= playingTimeMs && audioStart > nearestStart) {
          nearestStart = audioStart;
          nearest = p.id;
        }
      }
      return nearest;
    }
    return null;
  });

  // Scroll the currently-playing paragraph into view when it starts.
  // `nearest` means we only move the page when the paragraph is
  // actually off-screen — if it's already visible (typical during
  // continuous master playback through consecutive rows), this is a
  // no-op.
  $effect(() => {
    const id = currentPlayingParagraphId;
    if (id == null) return;
    // Wait a frame so the just-mounted `.playing` class + any layout
    // change (sticky pane-head, etc.) settles before we measure.
    requestAnimationFrame(() => {
      const el = document.querySelector(`[data-paragraph-id="${id}"]`);
      if (el) el.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    });
  });

  // ---- Progress predicates for the swipe animation ----

  /// A speech row is "master-playing" when the per-slot audio playhead
  /// is within `[row.audioStart, next-speech-row.audioStart)`.
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
  /// watches.
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
  /// meaning: "how close are we to the auto-skip cutoff".
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

  // Paragraph click → copy the full paragraph text to the clipboard.
  // `copiedParagraphId` pulses briefly on the just-copied block so the
  // user gets visual confirmation without a modal or toast. Cleared by
  // a timer.
  let copiedParagraphId = $state(null);
  let copyClearTimer = 0;
  const COPY_FEEDBACK_MS = 900;

  async function copyParagraphText(p) {
    try {
      await navigator.clipboard.writeText(p.text ?? '');
      copiedParagraphId = p.id;
      if (copyClearTimer) clearTimeout(copyClearTimer);
      copyClearTimer = setTimeout(() => {
        copiedParagraphId = null;
        copyClearTimer = 0;
      }, COPY_FEEDBACK_MS);
    } catch (e) {
      err = `copy failed: ${e.message}`;
    }
  }

  // Inline rename for the in-flight recording's pane header. Click the
  // name to enter edit mode; Enter/blur commits, Escape cancels.
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

  // Sticky-header collapse: an IntersectionObserver watches a 1px
  // sentinel placed just above the live pane's header. When the user
  // scrolls the sentinel past the top of the viewport (accounting for
  // the pinned Menubar via `rootMargin`), the header enters `compact`
  // mode — smaller title, tighter padding — so any pinned controls
  // stay visible without the title hogging the top strip on a long
  // transcript scroll.
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

  // Flatten the live pane's per-channel paragraph groups into a single
  // list for the shared cluster renderer. Wrapped in a synthesized
  // recording-like object so `clusterList` can operate uniformly across
  // live / active / saved.
  const liveRecording = $derived.by(() => {
    if (!state) return null;
    const paragraphs = (state.paragraphsByChannel ?? []).flatMap(
      (x) => x.paragraphs ?? [],
    );
    return { id: null, paragraphs, created_at: 0 };
  });

  // Map channel name → llm_inflight flag from the current snapshot.
  // Used by paragraphState to pulse the state dot on non-hardened
  // paragraphs while a pass-4 LLM call is in flight on their channel.
  // TTS channel is always false server-side.
  const channelInflight = $derived.by(() => {
    const out = {};
    for (const ch of state?.paragraphsByChannel ?? []) {
      out[ch.channel] = !!ch.llm_inflight;
    }
    return out;
  });

  // Compute the 5-state label for a paragraph. Rendered as a colored
  // dot in the paragraph header. The `pulsing` overlay is a separate
  // boolean (returned as .inflight below) so any state can pulse when
  // background work is touching this block.
  function paragraphState(p) {
    const clips = p.clips ?? [];
    if (clips.some((c) => c.provisional)) return 'streaming';
    if (p.hardened) return 'hardened';
    if (p.pass4_ran) return 'reorganized';
    // Pass 3 requires 2+ finalized clips (nothing to reconsolidate on
    // a single clip). A single-clip paragraph is terminal by
    // construction — display it as condensed to match multi-clip
    // paragraphs that went through pass 3.
    if (p.pass3_ran || clips.length === 1) return 'condensed';
    return 'finalized';
  }
  function paragraphInflight(p) {
    if (p.pass3_inflight) return true;
    if (!p.hardened && channelInflight[p.channel || channelFallback]) return true;
    return false;
  }
  const STATE_LABELS = {
    streaming: 'Streaming — clips still landing',
    finalized: 'Finalized — pass 3 pending',
    condensed: 'Condensed — boundary re-transcribed',
    reorganized: 'Reorganized — LLM rewrote this paragraph',
    hardened: 'Hardened — scrolled out of the LLM hot zone',
  };
  // Countdown until the paragraph auto-hardens by timeout — matches
  // the server-side `PARAGRAPH_GAP_MS` in record.rs (any new clip
  // arriving past this window opens a new paragraph anyway, so the
  // paragraph is effectively done). Ticks via the /record poll
  // (500 ms) so the value re-derives on every state refresh.
  const HARDEN_TIMEOUT_MS = 6000;
  function hardenCountdownSec(p) {
    if (!p || p.hardened) return null;
    const clips = p.clips ?? [];
    if (clips.length === 0) return null;
    if (clips.some((c) => c.provisional)) return null;
    const last = clips[clips.length - 1];
    const lastEnd =
      (last.start_wall_ms ?? 0) + (last.audio_duration_ms ?? 0);
    const remainingMs = lastEnd + HARDEN_TIMEOUT_MS - Date.now();
    if (remainingMs <= 0) return null;
    return Math.ceil(remainingMs / 1000);
  }
</script>

{#snippet hiddenAudios()}
  <!-- Both audio elements are hidden — playback is driven entirely by
       the clip chips + row master ▶ buttons. Master (per-slot audio)
       auto-advances through subsequent rows; Clip (single-segment) is
       one-shot. `ondurationchange` + `onloadedmetadata` mirror the
       browser's duration into `$state` so the Stop button's swipe fill
       reacts the moment metadata for a fresh track arrives. -->
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

{#snippet clipStrip(p, recId)}
  <!-- Compact per-clip strip pinned beneath each paragraph's prose.
       One chip per clip. Chip label is the clip's offset from the
       paragraph's start (e.g. "0:03") — for a single-clip TTS
       paragraph we show a bare ▸ instead, since the offset would
       always be 0:00. Live pane vox clips are transcript-only by
       design (no per-slot audio endpoint) — those render as
       passive tick marks instead of playable chips. -->
  <div class="clip-strip">
    {#each p.clips ?? [] as clip (clip.id)}
      {@const isTts = !!clip.audio_url}
      {@const playable = recId != null || isTts}
      {@const soloTts = isTts && (p.clips?.length ?? 0) === 1}
      {#if playable}
        <button
          type="button"
          class="clip-chip"
          class:playing={playingClipId === clip.id}
          class:provisional={clip.provisional}
          class:tts={isTts}
          title={clip.text || 'Play clip'}
          onclick={() => {
            if (playingClipId === clip.id) { stopPlayback(); return; }
            playClip(
              {
                id: clip.id,
                audio_url: clip.audio_url,
                audio_start_ms: clip.audio_start_ms,
                audio_duration_ms: clip.audio_duration_ms,
              },
              recId,
            );
          }}
        >
          {#if playingClipId === clip.id}
            <div class="clip-fill" style="width: {clipDurationMs ? Math.min(1, playingTimeMs / clipDurationMs) * 100 : 0}%" aria-hidden="true"></div>
          {/if}
          <span class="clip-chip-label">
            {#if soloTts}▸{:else}{fmtRelMs((clip.start_wall_ms ?? 0) - (p.start_wall_ms ?? 0))}{/if}
          </span>
        </button>
      {:else}
        <span
          class="clip-tick"
          class:provisional={clip.provisional}
          title={clip.text || ''}
        >
          {fmtRelMs((clip.start_wall_ms ?? 0) - (p.start_wall_ms ?? 0))}
        </span>
      {/if}
    {/each}
  </div>
{/snippet}

{#snippet paragraphBlock(p, recId)}
  {@const ch = p.channel || channelFallback}
  {@const isTts = ch === 'TTS'}
  {@const title = isTts ? (p.speaker || 'TTS') : (p.speaker || ch)}
  {@const channelType = isTts ? 'TTS' : 'VOX'}
  {@const pstate = paragraphState(p)}
  {@const pulsing = paragraphInflight(p)}
  {@const harden_secs = hardenCountdownSec(p)}
  <div
    class="paragraph-block"
    class:playing={currentPlayingParagraphId === p.id}
    class:provisional={!p.hardened && (p.clips ?? []).some((c) => c.provisional)}
  >
    <div class="paragraph-head">
      <div class="entry-avatar" title={fmtTime(p.created_at)}>
        <div class="avatar-portrait" style="background: {avatarTint(title)}">
          <span class="avatar-initial">{initialFrom(title)}</span>
        </div>
        <span class="avatar-type" class:tts={isTts}>{channelType}</span>
        <span class="avatar-title" title={title}>{title}</span>
      </div>
      <span
        class="state-dot state-{pstate}"
        class:pulsing
        title={STATE_LABELS[pstate]}
        aria-label={pstate}
      ></span>
      {#if harden_secs != null}
        <span class="harden-countdown" title="Auto-hardens in {harden_secs}s">
          {harden_secs}s
        </span>
      {/if}
      <span class="paragraph-ts" title={fmtTime(p.created_at)}>
        {fmtWallTime(p.start_wall_ms)}
      </span>
    </div>
    <button
      type="button"
      class="paragraph-text"
      data-paragraph-id={p.id}
      class:copied={copiedParagraphId === p.id}
      title={copiedParagraphId === p.id ? 'Copied!' : 'Click to copy paragraph text'}
      onclick={() => copyParagraphText(p)}
    >{p.text || ''}</button>
    {@render clipStrip(p, recId)}
  </div>
{/snippet}

{#snippet clusterList(recording, canonicalChannels)}
  {@const rows = computeClusters(recording.paragraphs, canonicalChannels)}
  {@const recId = recording.id}
  {#if rows.length === 0}
    <div class="empty pane-empty">no transcript yet</div>
  {:else}
    <ol class="clusters">
      {#each rows as row (row.kind + ':' + row.wallStart)}
        <li>
          {#if row.kind === 'silence'}
            <div class="cluster silence-row">
              <button
                type="button"
                class="silence-tag master-btn silence"
                class:playing={isSilenceRowPlaying(row, rows)}
                onclick={() => recId != null && playMasterFromRow(row, rows, recId)}
                title={recId != null
                  ? 'Skip silence — jump master to next row'
                  : 'Silence gap (live view — no master audio yet)'}
                disabled={recId == null}
              >
                {#if isSilenceRowPlaying(row, rows)}
                  <div class="clip-fill" style="width: {silenceRowProgress(row, rows) * 100}%" aria-hidden="true"></div>
                {/if}
                <span class="silence-text">silence · {Math.max(0, Math.round((row.wallEnd - row.wallStart) / 1000))}s</span>
              </button>
            </div>
          {:else if row.channels.length === 1}
            {@const ch = row.channels[0]}
            <div class="cluster full">
              {#if recId != null && row.audioStart != null}
                <button
                  type="button"
                  class="master-btn row-master"
                  class:playing={isRowMasterPlaying(row, rows)}
                  onclick={() => playMasterFromRow(row, rows, recId)}
                  title="Play the recording from here"
                >
                  {#if isRowMasterPlaying(row, rows)}
                    <div class="clip-fill" style="width: {rowMasterProgress(row, rows) * 100}%" aria-hidden="true"></div>
                  {/if}
                  <span class="master-glyph">▶</span>
                </button>
              {/if}
              {#each row.byChannel[ch] ?? [] as p (p.id)}
                {@render paragraphBlock(p, recId)}
              {/each}
            </div>
          {:else}
            <div class="cluster split" style="grid-template-columns: repeat({row.channels.length}, 1fr)">
              {#if recId != null && row.audioStart != null}
                <button
                  type="button"
                  class="master-btn row-master row-master-split"
                  class:playing={isRowMasterPlaying(row, rows)}
                  onclick={() => playMasterFromRow(row, rows, recId)}
                  title="Play the recording from here"
                  style="grid-column: 1 / -1;"
                >
                  {#if isRowMasterPlaying(row, rows)}
                    <div class="clip-fill" style="width: {rowMasterProgress(row, rows) * 100}%" aria-hidden="true"></div>
                  {/if}
                  <span class="master-glyph">▶</span>
                </button>
              {/if}
              {#each row.channels as ch (ch)}
                <div class="cluster-col">
                  {#each row.byChannel[ch] ?? [] as p (p.id)}
                    {@render paragraphBlock(p, recId)}
                  {/each}
                </div>
              {/each}
            </div>
          {/if}
        </li>
      {/each}
    </ol>
  {/if}
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
      <!-- Rolling ephemeral live view. Sentinel is a 1px shim sitting
           just above the sticky header so IntersectionObserver can flip
           the header into its `compact` mode the moment scroll pins it
           to the top. -->
      <div bind:this={liveHeadSentinel} class="head-sentinel"></div>
      <div class="pane-head live" class:compact={liveHeadCompact}>
        <h1 title="Ephemeral live transcript (buffered; not recorded)">
          <span class="live-title-long">Ephemeral live transcript (buffered; not recorded)</span>
          <span class="live-title-short">Live transcript</span>
          {#if !state.sttEnabled}
            <span class="stt-off"> · STT disabled</span>
          {/if}
        </h1>
        <label
          class="llm-toggle"
          title="Off (default): the live pane is a plain per-clip transcript. On: the LLM reinterprets paragraphs as they land, using GPU time. Ignored during a named recording — the LLM always runs there."
        >
          <input
            type="checkbox"
            bind:checked={llmWhenIdle}
            onchange={onLlmWhenIdleChange}
          />
          <span>Reinterpret with LLM</span>
        </label>
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
      {@render hiddenAudios()}
      {#if !liveRecording || liveRecording.paragraphs.length === 0}
        <div class="empty pane-empty">
          nothing spoken yet — the {channelFallback} channel is quiet
        </div>
      {:else}
        {@render clusterList(liveRecording, state.channelNames ?? [])}
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
              title={state.activeRecording.name}
              onclick={beginActiveRename}
            >{state.activeRecording.name}</button>
          {/if}
          <span class="active-time">{fmtDur(state.activeRecording.duration_ms ?? 0)}</span>
        </div>
        <div class="active-actions">
          {@render playAllBtn(state.activeRecording.id)}
          <button type="button" class="primary" disabled={busy} onclick={stopRecording}>
            Stop &amp; Save
          </button>
        </div>
      </div>
      {@render hiddenAudios()}
      {#if (state.activeRecording.paragraphs ?? []).length === 0}
        <div class="empty pane-empty">listening…</div>
      {:else}
        {@render clusterList(state.activeRecording, state.channelNames ?? [])}
      {/if}
    {:else if selectedSaved}
      <!-- Saved recording detail. Kept as a single-line top-aligned bar
           so it doesn't push the log down. Title is click-to-rename. -->
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
            title={selectedSaved.name}
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
      {#if (selectedSaved.paragraphs ?? []).length === 0}
        <div class="empty pane-empty">no transcript for this recording</div>
      {:else}
        {@render clusterList(selectedSaved, state.channelNames ?? [])}
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
  /* Live pane variant: locked to a single row (no `flex-wrap`) so the
     title and Record button never wrap on narrow / low-res screens —
     the h1 shrinks and ellipsizes instead. */
  .pane-head.live {
    align-items: center;
    flex-wrap: nowrap;
    min-width: 0;
  }
  .pane-head.live h1 {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Swap the long title for a short one below ~560px viewport. */
  .live-title-short { display: none; }
  @media (max-width: 560px) {
    .live-title-long { display: none; }
    .live-title-short { display: inline; }
  }
  /* Start-recording affordance pinned to the far-right corner of the
     sticky header. Deliberately understated in its resting state — a
     small red dot as an icon, muted border, no glow, no animation. */
  /* "Reinterpret with LLM" checkbox — sits between the title and the
     Record button. `margin-left: auto` on this element pushes the
     Record button flush right; the toggle stays packed to the left of
     it so the compact header still fits on narrow widths. */
  .pane-head.live .llm-toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 8px;
    border-radius: 999px;
    color: var(--muted);
    font-size: 12px;
    cursor: pointer;
    flex: 0 0 auto;
    margin-left: auto;
    user-select: none;
  }
  .pane-head.live .llm-toggle:hover {
    color: var(--text);
  }
  .pane-head.live .llm-toggle input[type="checkbox"] {
    margin: 0;
    accent-color: rgba(122,162,255,0.8);
  }
  .pane-head.live.compact .llm-toggle {
    /* In the compact (scrolled) header the space is tight; drop the
       label but keep the checkbox so the toggle is still available. */
    padding: 3px 4px;
  }
  .pane-head.live.compact .llm-toggle span {
    display: none;
  }
  .pane-head.live .live-rec-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border: 1px solid rgba(255,255,255,0.15);
    border-radius: 999px;
    background: transparent;
    color: var(--fg-2, #cfd3d8);
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
    flex: 0 0 auto;
    transition: background 0.15s ease, border-color 0.15s ease, color 0.15s ease;
  }
  .pane-head.live .live-rec-btn:hover:not(:disabled) {
    background: rgba(255,92,92,0.1);
    border-color: rgba(255,92,92,0.55);
    color: #ff8a8a;
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
    opacity: 0.75;
  }
  /* Compact-header form: drop the label so the button becomes a
     circular red dot in the corner. */
  .pane-head.compact .live-rec-btn { padding: 3px 6px; }
  .pane-head.compact .live-rec-label { display: none; }
  /* Compact form — activated once the header is pinned to the top. */
  .pane-head.compact {
    padding-top: 2px;
    padding-bottom: 2px;
  }
  .pane-head.compact h1 { font-size: 12px; }
  @media (max-width: 1079px) {
    .pane-head { padding-left: 44px; }
  }
  .pane-head h1 { font-size: 16px; margin: 0; line-height: 1.2; }

  /* Saved-recording variant. */
  .pane-head.saved {
    align-items: center;
    flex-wrap: nowrap;
    padding: 6px 4px;
  }
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
     presence doesn't shift the log around when playback starts. */
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
    position: relative;
    overflow: hidden;
  }
  .topbar-btn.stop:hover { background: rgba(255,92,92,0.24); }
  /* Swipe overlay showing playback progress. */
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
    transition: width 60ms linear;
  }
  .stop-label {
    position: relative;
    z-index: 1;
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

  /* ---- Paragraph cluster layout ----
     One <li> per cluster. Speech clusters become either a full-width
     column (single participating channel) or a grid split into N equal
     columns (one per channel actively speaking during the cluster).
     Silence clusters are a single lozenge showing the gap length + a
     master-scrub affordance. */
  ol.clusters {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  ol.clusters > li {
    display: block;
    padding: 0;
  }
  .cluster {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    display: block;
    min-width: 0;
  }
  .cluster.split {
    display: grid;
    gap: 10px;
    align-items: start;
  }
  .cluster-col {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 0 4px;
    border-right: 1px dashed rgba(255,255,255,0.06);
  }
  .cluster.split .cluster-col:last-child { border-right: none; }
  /* Silence cluster: single lozenge lozenge affordance. */
  .cluster.silence-row {
    display: flex;
    padding: 6px 10px;
    background: transparent;
    border-color: transparent;
  }
  .row-master {
    /* Small ▶ pinned in the cluster header so a click plays from the
       cluster's audioStart onward. Absent when the cluster has no
       positioned audio (live pane or TTS-only). */
    float: right;
    margin-left: 8px;
    margin-bottom: 4px;
  }
  .row-master-split {
    float: none;
    margin: 0 0 8px 0;
    justify-self: start;
  }

  /* Paragraph block: avatar header + prose button + clip strip. */
  .paragraph-block {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 4px 0;
    min-width: 0;
  }
  .paragraph-block + .paragraph-block {
    border-top: 1px solid rgba(255,255,255,0.04);
    padding-top: 8px;
  }
  .paragraph-block.playing .paragraph-text {
    border-color: rgba(122,162,255,0.6);
    background: rgba(122,162,255,0.06);
  }
  .paragraph-block.provisional { opacity: 0.78; }
  .paragraph-block.provisional .paragraph-text { font-style: italic; }
  .paragraph-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .paragraph-ts {
    color: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    margin-left: auto;
  }
  /* State indicator: a small colored circle in the paragraph header
     showing the paragraph's current processing state.
       streaming   — red    (clips still landing)
       finalized   — orange (pass 2 done, pass 3 hasn't fired)
       condensed   — amber  (pass 3 boundary re-transcribe done)
       reorganized — yellow (LLM has rewritten this paragraph, still in hot zone)
       hardened    — grey   (scrolled out of hot zone, immutable)
     The `pulsing` overlay class animates opacity while background
     work is touching this paragraph (pass-3 decode in flight, or the
     channel's LLM cycle is running on its hot zone). */
  .state-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--muted);
    flex-shrink: 0;
    box-shadow: 0 0 0 1px rgba(0,0,0,0.35);
  }
  .state-dot.state-streaming   { background: #e35555; }
  .state-dot.state-finalized   { background: #e68a3a; }
  .state-dot.state-condensed   { background: #d4b93a; }
  .state-dot.state-reorganized { background: #e0c02a; }
  .state-dot.state-hardened    { background: #888888; }
  .state-dot.pulsing {
    animation: state-dot-pulse 1.2s ease-in-out infinite;
  }
  @keyframes state-dot-pulse {
    0%, 100% { opacity: 1; box-shadow: 0 0 0 1px rgba(0,0,0,0.35); }
    50%      { opacity: 0.35; box-shadow: 0 0 0 4px rgba(255,255,255,0.08); }
  }
  /* Countdown label sits just after the state dot until the paragraph
     auto-hardens by timeout. Tabular numerals so the width doesn't
     shift as the seconds tick down. */
  .harden-countdown {
    color: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;
    user-select: none;
  }
  .paragraph-text {
    display: block;
    width: 100%;
    background: transparent;
    color: var(--text);
    border: 1px dashed transparent;
    padding: 4px 6px;
    font: inherit;
    font-size: 14px;
    line-height: 1.45;
    text-align: left;
    cursor: pointer;
    border-radius: 4px;
    min-width: 0;
    word-break: break-word;
    white-space: normal;
  }
  .paragraph-text:hover {
    border-color: rgba(122,162,255,0.4);
    background: rgba(122,162,255,0.06);
  }
  .paragraph-text.copied {
    border-color: rgba(46, 204, 74, 0.55);
    background: rgba(46, 204, 74, 0.12);
  }

  /* Clip strip — a compact row of chips below each paragraph. */
  .clip-strip {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 0 6px;
  }
  .clip-chip {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 40px;
    padding: 2px 8px;
    background: rgba(122,162,255,0.08);
    color: var(--accent);
    border: 1px solid rgba(122,162,255,0.3);
    border-radius: 999px;
    font: inherit;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;
    cursor: pointer;
    overflow: hidden;
  }
  .clip-chip:hover:not(:disabled) {
    background: rgba(122,162,255,0.16);
    color: var(--text);
  }
  .clip-chip:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .clip-chip.tts {
    background: rgba(255,204,102,0.08);
    color: #ffcc66;
    border-color: rgba(255,204,102,0.4);
  }
  .clip-chip.tts:hover:not(:disabled) {
    background: rgba(255,204,102,0.16);
  }
  .clip-chip.playing {
    border-color: rgba(122,162,255,0.75);
    box-shadow: 0 0 0 1px rgba(122,162,255,0.35);
  }
  .clip-chip.provisional {
    font-style: italic;
    opacity: 0.7;
  }
  .clip-chip-label {
    position: relative;
    z-index: 1;
  }

  /* Passive clip marker for the live pane, where per-clip audio has
     nowhere to come from. Same label shape as a chip so users can
     still see the LLM's paragraph boundaries at a glance, but no
     button semantics, no hover, no cursor. */
  .clip-tick {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 40px;
    padding: 2px 8px;
    color: var(--muted);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    line-height: 1.2;
    opacity: 0.55;
    user-select: none;
  }
  .clip-tick.provisional {
    font-style: italic;
    opacity: 0.4;
  }

  /* Animated fill: absolute overlay behind the chip content whose
     width is bound to per-clip playback progress and updated per rAF
     tick. */
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

  /* Avatar chip — voice/channel identity for the paragraph header. */
  .entry-avatar {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 6px;
    background: rgba(0, 0, 0, 0.28);
    border: 1px solid rgba(255, 255, 255, 0.05);
    border-radius: 5px;
    text-align: left;
    overflow: hidden;
    min-width: 0;
  }
  .avatar-portrait {
    width: 22px;
    height: 22px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: rgba(255, 255, 255, 0.9);
    font-size: 12px;
    font-weight: 600;
    line-height: 1;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.08);
    user-select: none;
    flex-shrink: 0;
  }
  .avatar-initial {
    letter-spacing: 0;
  }
  .avatar-type {
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--accent);
    line-height: 1;
  }
  .avatar-type.tts { color: #ffcc66; }
  .avatar-title {
    font-size: 11px;
    line-height: 1.15;
    color: var(--text);
    max-width: 14ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Silence tag — used in silence-row clusters. */
  .silence-tag {
    display: inline-block;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    background: rgba(255,255,255,0.05);
    padding: 4px 10px;
    border-radius: 4px;
    position: relative;
    overflow: hidden;
    cursor: pointer;
  }
  .silence-tag.playing {
    color: var(--text);
    background: rgba(122,162,255,0.14);
  }
  .silence-tag .clip-fill {
    z-index: 0;
  }
  .silence-text {
    position: relative;
    z-index: 1;
  }

  /* Master row button — the ▶ that plays the per-slot audio from the
     cluster's audioStart onward. */
  .master-btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 44px;
    height: 24px;
    padding: 0 8px;
    background: rgba(122,162,255,0.12);
    color: var(--accent);
    border: 1px solid rgba(122,162,255,0.4);
    border-radius: 4px;
    cursor: pointer;
    overflow: hidden;
  }
  .master-btn:hover:not(:disabled) { background: rgba(122,162,255,0.22); }
  .master-btn:disabled { opacity: 0.4; cursor: not-allowed; }
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
