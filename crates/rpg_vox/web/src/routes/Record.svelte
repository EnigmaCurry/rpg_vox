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

  // Selected sidebar item: 'live' (rolling buffer), 'active' (in-flight
  // recording), or a recording id (saved). Recording auto-selects to
  // 'active' on start; stop drops back to 'live' unless the user is
  // already looking at a saved one.
  let selected = $state('live');
  let previouslyActive = false;

  const POLL_MS = 500;
  let timer = null;

  // Wall-clock tick for relative timestamps ("Ns ago" / "Nm ago"). We
  // tick every second so the label updates promptly when it crosses a
  // bucket boundary; the formatter itself rounds to 10s so the visible
  // number only steps in 10s increments.
  let nowMs = $state(Date.now());
  let nowTimer = null;

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
    nowTimer = setInterval(() => { nowMs = Date.now(); }, 1000);
  });

  onDestroy(() => {
    if (timer) { clearInterval(timer); timer = null; }
    if (nowTimer) { clearInterval(nowTimer); nowTimer = null; }
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
    if (flashClearTimer) { clearTimeout(flashClearTimer); flashClearTimer = 0; }
  });

  async function refresh() {
    try {
      state = await api.getRecordState();
      err = null;
    } catch (e) {
      err = e.message;
      return;
    }
    applyServerPlayback(state.playback ?? null);
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
  // (a local-time timestamp). The user can click-to-rename in the
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
  // Relative wall time. Under a minute → "Ns ago" bucketed to the
  // nearest 10s (0s, 10s, 20s, …, 50s). Under an hour → "Nm ago". At
  // or beyond an hour → the absolute HH:MM:SS, since "62m ago" is
  // harder to parse than the actual clock time.
  function fmtWallTimeRel(ms, now) {
    if (!ms) return '';
    const diff = Math.max(0, now - ms);
    if (diff < 60_000) {
      return `${Math.floor(diff / 10_000) * 10}s ago`;
    }
    if (diff < 3_600_000) {
      return `${Math.floor(diff / 60_000)}m ago`;
    }
    return fmtWallTime(ms);
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
  const SILENCE_ROW_MIN_MS = 5 * 60 * 1000;

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
    const nowMs = Date.now();
    // "Latest paragraph on channel" is stable — once a newer paragraph
    // opens on the same channel, the server imperatively hardens the
    // previous one and it can never come back to being the latest. In
    // contrast, `p.hardened` in the snapshot is *re-derived* on every
    // render from a 2 s timeout, so during natural pauses in speech
    // the current paragraph's `hardened` flips true, then back to
    // false when the next chunk lands. Basing "in-flight" on that
    // flag directly makes the split-cluster rendering oscillate.
    const latestByChannel = {};
    for (const p of paragraphs ?? []) {
      const ch = p.channel || 'Unknown';
      const cur = latestByChannel[ch];
      if (!cur || (p.start_wall_ms ?? 0) > (cur.start_wall_ms ?? 0)) {
        latestByChannel[ch] = p;
      }
    }
    // A recording's latest paragraph on a channel counts as in-flight
    // only while its last-clip end is fresh — after this window we
    // treat it as hardened even if a new chunk could theoretically
    // still arrive. Long enough to cover natural pauses inside a
    // speaking turn; short enough that a genuinely finished paragraph
    // eventually settles into per-clip rendering.
    const RECENT_ACTIVITY_MS = 30000;
    const isInFlight = (p) => {
      if (latestByChannel[p.channel || 'Unknown'] !== p) return false;
      return (p.end_wall_ms ?? 0) > nowMs - RECENT_ACTIVITY_MS;
    };
    const effectiveEnd = (p) => {
      const base = p.end_wall_ms ?? 0;
      return isInFlight(p) ? Math.max(base, nowMs) : base;
    };
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
      const channels = orderChannels(chSet, canonical);
      // For split clusters (multiple participating channels), each
      // entry becomes one grid cell. In-flight (not-yet-hardened)
      // paragraphs are emitted as a single paragraph-level entry: while
      // chunks are still landing, cross-channel alignment must stay
      // stable, so we treat the whole paragraph as one unit rather than
      // exposing its per-chunk boundaries (which would cause a brief
      // utterance on another channel to jump between slots as each
      // Vox2 chunk finalizes and its cEnd shrinks). Hardened paragraphs
      // fan out into per-clip entries so a brief utterance can align
      // with the specific clip it overlaps.
      const orderedClips = [];
      for (const p of cluster) {
        const clipList = p.clips ?? [];
        const colIndex = channels.indexOf(p.channel || 'Unknown');
        if (isInFlight(p)) {
          const lastClip = clipList[clipList.length - 1];
          const lastEnd =
            (lastClip?.start_wall_ms ?? p.start_wall_ms ?? wallStart) +
            (lastClip?.audio_duration_ms ?? 0);
          orderedClips.push({
            p,
            clip: null,
            colIndex,
            cStart: p.start_wall_ms ?? wallStart,
            paragraphCEnd: lastEnd,
            isInFlight: true,
            isFirstOfP: true,
            isLastOfP: true,
          });
          continue;
        }
        if (clipList.length === 0) {
          orderedClips.push({
            p,
            clip: null,
            colIndex,
            cStart: p.start_wall_ms ?? wallStart,
            isFirstOfP: true,
            isLastOfP: true,
          });
          continue;
        }
        for (let ci = 0; ci < clipList.length; ci++) {
          const clip = clipList[ci];
          orderedClips.push({
            p,
            clip,
            colIndex,
            cStart: clip.start_wall_ms ?? p.start_wall_ms ?? wallStart,
            isFirstOfP: ci === 0,
            isLastOfP: ci === clipList.length - 1,
          });
        }
      }
      orderedClips.sort((a, b) => a.cStart - b.cStart);
      // Slot assignment: temporally-overlapping clips on *different*
      // channels share a grid row so a brief utterance on Vox1 that
      // arrives during Vox2's still-recording clip lands beside it —
      // not below. A provisional clip's effective end is stretched to
      // `now` so overlapping utterances share its row even before
      // its audio_duration_ms is known / transcription finalizes.
      // Same-channel clips never share a slot (would collide in the
      // same grid column).
      let currentSlot = null;
      let numSlots = 0;
      for (const entry of orderedClips) {
        const cStart = entry.cStart;
        let cEnd;
        if (entry.isInFlight) {
          // In-flight paragraph entry: extend to `now` so overlapping
          // utterances on other channels can share this slot regardless
          // of how far behind transcription is lagging.
          cEnd = Math.max(entry.paragraphCEnd ?? cStart, nowMs);
        } else {
          const cDur = entry.clip?.audio_duration_ms ?? 0;
          const rawEnd = cStart + cDur;
          const isProvisional = !!entry.clip?.provisional;
          cEnd = isProvisional ? Math.max(rawEnd, nowMs) : rawEnd;
        }
        const ch = entry.p.channel || 'Unknown';
        const canJoin =
          currentSlot &&
          cStart <= currentSlot.maxEnd &&
          !currentSlot.channelsUsed.has(ch);
        if (!canJoin) {
          numSlots += 1;
          currentSlot = {
            slotIdx: numSlots,
            maxEnd: cEnd,
            channelsUsed: new Set([ch]),
          };
        } else {
          currentSlot.maxEnd = Math.max(currentSlot.maxEnd, cEnd);
          currentSlot.channelsUsed.add(ch);
        }
        entry.slotIdx = currentSlot.slotIdx;
      }
      const orderedParagraphs = cluster.map((p) => ({
        p,
        colIndex: channels.indexOf(p.channel || 'Unknown'),
      }));
      rows.push({
        kind: 'speech',
        wallStart,
        wallEnd,
        audioStart,
        byChannel,
        channels,
        orderedParagraphs,
        orderedClips,
        numSlots,
      });
      cluster = [];
      clusterEnd = 0;
    };
    for (const p of sorted) {
      const pStart = p.start_wall_ms ?? 0;
      const pEnd = effectiveEnd(p);
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
      // Nominal gap in wall-clock time. Any paragraph that is still
      // in-flight (not hardened) should not be visually sliced off
      // from adjacent content by a silence break — during recording,
      // everything currently being spoken belongs in the same cluster
      // so brief overlapping utterances on *different* channels land
      // beside the recording clip rather than in a separated column-
      // form cluster. If only the *cluster* side is in-flight, keep a
      // bounded merge window so a stale unhardened paragraph doesn't
      // pull in unrelated later content indefinitely.
      //
      // The merge is restricted to cross-channel cases: two consecutive
      // paragraphs on the *same* channel are chronological, not
      // simultaneous, so they belong in their own clusters and get the
      // uniform inter-cluster gap. Otherwise the tail of a live log
      // shows same-channel pairs glued together inside one card while
      // every other pair has the normal 8px break.
      const CLUSTER_INFLIGHT_MERGE_MAX_MS = 30000;
      const clusterHasInFlight = cluster.some(isInFlight);
      const pIsInFlight = isInFlight(p);
      const gapMs = pStart - clusterEnd;
      const pCh = p.channel || 'Unknown';
      const sameChannelInCluster = cluster.some(
        (cp) => (cp.channel || 'Unknown') === pCh,
      );
      if (
        !sameChannelInCluster &&
        (pIsInFlight ||
          (clusterHasInFlight && gapMs < CLUSTER_INFLIGHT_MERGE_MAX_MS))
      ) {
        cluster.push(p);
        if (pEnd > clusterEnd) clusterEnd = pEnd;
        continue;
      }
      // Real gap between fully-hardened clusters — flush and emit
      // silence if the gap is long enough.
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

  // ---- Server-driven playback state ----
  //
  // Playback runs server-side through the mixer/PipeWire virtual mic —
  // that way Discord actually hears what the user auditions, unlike the
  // old browser `<audio>` path which only reached the local speakers.
  //
  // The client just issues POSTs (start, stop) and reads a `playback`
  // field off each /record poll (every 500 ms) for authoritative
  // position + duration. `tickPlayhead` extrapolates forward from the
  // most recent server sample using `performance.now()` so the swipe
  // animation stays smooth at 60 fps without hammering the endpoint.
  //
  // Regression from the old <audio> version: silence-skip during "Play
  // All" is now click-only. Server playback can't be seeked mid-burst
  // (the ring is a one-shot push), so a long dead-air section plays
  // through until the user Stops and clicks the next row's ▶.
  let playingRecordingId = $state(null);
  let playingClipId = $state(null);
  let playbackMode = $state(null);          // 'master' | 'clip' | null
  let playingTimeMs = $state(0);            // ms — highlight + swipe cursor
  let playbackDurationMs = $state(0);       // ms — from server; used to bound fills
  // Most recent server position sample plus the client wall-clock at
  // which we received it, for smooth extrapolation between polls.
  let playbackSample = $state(null);
  let rafId = 0;

  function tickPlayhead() {
    rafId = 0;
    if (playbackMode != null && playbackSample) {
      const elapsed = performance.now() - playbackSample.recvPerfMs;
      const target = playbackSample.serverPositionMs + elapsed;
      playingTimeMs = playbackDurationMs
        ? Math.min(target, playbackDurationMs)
        : target;
    }
    if (playbackMode != null) {
      rafId = requestAnimationFrame(tickPlayhead);
    }
  }

  function ensureRaf() {
    if (!rafId && playbackMode != null) {
      rafId = requestAnimationFrame(tickPlayhead);
    }
  }

  /// Fold a `/record` snapshot's `playback` field into the client's
  /// local state. `pb === null` means "nothing playing" and clears the
  /// highlight. Called from every refresh() so the state stays honest.
  function applyServerPlayback(pb) {
    if (!pb) {
      playingRecordingId = null;
      playingClipId = null;
      playbackMode = null;
      playingTimeMs = 0;
      playbackDurationMs = 0;
      playbackSample = null;
      return;
    }
    playingRecordingId = pb.recordingId ?? null;
    playingClipId = pb.clipId ?? null;
    playbackMode = pb.mode ?? null;
    playbackDurationMs = pb.durationMs ?? 0;
    playbackSample = {
      serverPositionMs: pb.positionMs ?? 0,
      recvPerfMs: performance.now(),
    };
    playingTimeMs = pb.positionMs ?? 0;
    ensureRaf();
  }

  /// Resolve a recording by id from the current state snapshot,
  /// checking both the in-flight active bucket and the saved list.
  function findRecording(id) {
    if (!state || id == null) return null;
    if (state.activeRecording?.id === id) return state.activeRecording;
    return state.recordings?.find((r) => r.id === id) ?? null;
  }

  /// Whichever recording the mixer is currently reading from, if any.
  /// Used by the paragraph-highlight logic to look up per-clip audio
  /// ranges in the right transcript.
  function currentPlayingRecording() {
    return findRecording(playingRecordingId);
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

  /// Progress (0..1) for the Stop button's swipe fill.
  function stopBtnProgress() {
    if (!playbackDurationMs) return 0;
    return Math.max(0, Math.min(1, playingTimeMs / playbackDurationMs));
  }

  async function playMasterAtMs(recordingId, startMs) {
    // Optimistic UI: switch highlight immediately so the button
    // transitions to Stop without waiting the ~1 RTT for the POST +
    // ~500 ms for the next /record poll to arrive.
    playingRecordingId = recordingId;
    playingClipId = null;
    playbackMode = 'master';
    playingTimeMs = startMs;
    playbackDurationMs = 0;
    playbackSample = null;
    ensureRaf();
    try {
      await api.playRecording(recordingId, { startMs });
      await refresh();
    } catch (e) {
      err = e.message;
      applyServerPlayback(null);
    }
  }

  /// Master-column click for a row. Speech rows play from their own
  /// `audioStart`; silence rows jump to the next speech row so the
  /// master column doubles as a scrub-forward affordance.
  function playMasterFromRow(row, rows, recordingId) {
    if (row.kind === 'speech') {
      if (row.audioStart != null) playMasterAtMs(recordingId, row.audioStart);
      return;
    }
    const idx = rows.indexOf(row);
    for (let i = idx + 1; i < rows.length; i++) {
      if (rows[i].kind === 'speech' && rows[i].audioStart != null) {
        playMasterAtMs(recordingId, rows[i].audioStart);
        return;
      }
    }
    stopPlayback();
  }

  /// One-shot playback of a single clip through the mixer.
  ///
  /// Routing:
  ///   * Vox clip (saved/active recording): POST /record/recordings/:id/
  ///     playback with startMs + durationMs + clipId → server slices
  ///     the parent recording's audio.wav.
  ///   * TTS clip in a recording: same endpoint with widgetId + clipId
  ///     — server reads the widget's cached WAV instead.
  ///   * TTS clip in the live pane (no recording context): fall back to
  ///     /widgets/:id/say directly. Audio still reaches the mic but
  ///     there's no position tracker, so the chip's swipe fill won't
  ///     animate here.
  ///   * Vox clip in the live pane: unplayable (no server-side audio
  ///     endpoint for the rolling buffer); the chip is disabled at the
  ///     callsite.
  async function playClip(clipLike, recordingId) {
    let opts;
    if (clipLike.audio_url) {
      const m = String(clipLike.audio_url).match(/\/widgets\/([^\/?]+)/);
      const widgetId = m ? m[1] : null;
      if (!widgetId) return;
      if (recordingId == null) {
        // Live-pane TTS chip: no recording context to attach the
        // tracker to. Route through widget_say for the mixer path;
        // the swipe fill won't animate here but the audio still
        // reaches Discord.
        playingClipId = clipLike.id;
        playbackMode = 'clip';
        playingRecordingId = null;
        playingTimeMs = 0;
        playbackDurationMs = 0;
        playbackSample = null;
        ensureRaf();
        try {
          await fetch(`/widgets/${encodeURIComponent(widgetId)}/say`, { method: 'POST' });
        } catch (e) {
          err = e.message;
        }
        applyServerPlayback(null);
        return;
      }
      opts = { widgetId, clipId: clipLike.id };
    } else if (
      recordingId != null &&
      clipLike.audio_start_ms != null &&
      clipLike.audio_duration_ms != null
    ) {
      opts = {
        startMs: clipLike.audio_start_ms,
        durationMs: clipLike.audio_duration_ms,
        clipId: clipLike.id,
      };
    } else {
      return;
    }
    playingRecordingId = recordingId;
    playingClipId = clipLike.id;
    playbackMode = 'clip';
    playingTimeMs = 0;
    playbackDurationMs = 0;
    playbackSample = null;
    ensureRaf();
    try {
      await api.playRecording(recordingId, opts);
      await refresh();
    } catch (e) {
      err = e.message;
      applyServerPlayback(null);
    }
  }

  async function stopPlayback() {
    applyServerPlayback(null);
    try {
      await api.stopPlayback();
    } catch (e) {
      err = e.message;
    }
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
  /// speech row's audio start.
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
  /// the wall-clock gap between the flanking speech rows so a short
  /// silence still fills fully and a long silence still crawls.
  function silenceRowProgress(row, rows) {
    if (!isSilenceRowPlaying(row, rows)) return 0;
    const idx = rows.indexOf(row);
    const prev = prevSpeechRow(rows, idx);
    const next = nextSpeechRow(rows, idx);
    if (!prev || !next) return 0;
    const prevEnd = prev.audioStart + (prev.wallEnd - prev.wallStart);
    const elapsed = playingTimeMs - prevEnd;
    const dur = Math.max(1, next.audioStart - prevEnd);
    return Math.max(0, Math.min(1, elapsed / dur));
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

  // Reset playback whenever the selected recording changes so a stale
  // highlight from a previous pane doesn't linger.
  $effect(() => {
    // Reference `selected` to make this effect track it.
    selected;
    if (playingRecordingId !== null && playingRecordingId !== selected && playingRecordingId !== 'active') {
      stopPlayback();
    }
  });

  // Timestamp click → copy the full local timestamp string and briefly
  // swap the label to "copied" so the user sees the click landed. Keyed
  // by whatever id the clicker passed in (paragraph id or clip id) so
  // only that one button flashes.
  let copiedTsId = $state(null);
  let tsCopyClearTimer = 0;
  const TS_COPY_FLASH_MS = 700;
  async function copyTimestamp(id, fullStr) {
    try {
      await navigator.clipboard.writeText(fullStr);
      copiedTsId = id;
      if (tsCopyClearTimer) clearTimeout(tsCopyClearTimer);
      tsCopyClearTimer = setTimeout(() => {
        copiedTsId = null;
        tsCopyClearTimer = 0;
      }, TS_COPY_FLASH_MS);
    } catch (e) {
      err = `copy failed: ${e.message}`;
    }
  }

  // Two forms of the record-log action toolbar. When the mouseup lands
  // with a real text selection inside a paragraph/clip, `selTool` floats
  // next to the cursor. When the mouseup lands inside a paragraph/clip
  // *without* a selection (a plain click), `inlineTool` renders inline
  // at the end of that paragraph/clip and wraps under it on a new line.
  // Only one of the two is visible at a time.
  let selTool = $state({ visible: false, x: 0, y: 0, text: '' });
  let inlineTool = $state({ key: null, text: '' });
  // Brief background flash on the just-copied paragraph/clip text so
  // the user sees the copy landed. Keyed by the same `p:<id>` or
  // `clip:<id>` string used by `inlineTool.key`.
  let flashKey = $state(null);
  let flashClearTimer = 0;
  const FLASH_MS = 700;

  function selectionInsideRecordText() {
    const sel = window.getSelection?.();
    if (!sel || sel.rangeCount === 0) return null;
    const text = sel.toString();
    if (!text.trim()) return null;
    const anchor = sel.anchorNode;
    let el = anchor?.nodeType === 3 ? anchor.parentElement : anchor;
    while (el) {
      const cl = el.classList;
      if (cl && (cl.contains('paragraph-text') || cl.contains('clip-cell-text'))) {
        return text;
      }
      el = el.parentElement;
    }
    return null;
  }

  const TOOLBAR_SEL = '.selection-toolbar, .inline-selection-toolbar, .paragraph-editor';

  function onDocMouseUp(ev) {
    // Ignore mouseups that end on either toolbar or inside an open
    // editor — the respective button click handlers own that path,
    // and a mouseup after typing into the textarea shouldn't reopen
    // a selection toolbar.
    if (ev.target?.closest?.(TOOLBAR_SEL)) return;
    // When an editor is open, mouseups elsewhere in the pane must not
    // spawn either toolbar — the editor owns the interaction until
    // the user Applies or Cancels.
    if (editing.hostKey) return;
    // Selection is not always finalized synchronously on mouseup in
    // every browser; let it settle a tick before reading it.
    queueMicrotask(() => {
      const text = selectionInsideRecordText();
      if (text) {
        selTool.text = text;
        selTool.x = ev.clientX + 8;
        selTool.y = ev.clientY + 4;
        selTool.visible = true;
        inlineTool.key = null;
        return;
      }
      selTool.visible = false;
      const host = ev.target?.closest?.('.paragraph-text, .clip-cell-text');
      if (host && host.dataset.toolKey) {
        inlineTool.key = host.dataset.toolKey;
        // Prefer the currently-rendered text (which reflects any prior
        // edits applied on top of `p.text`) over the raw transcription
        // stored in data-tool-text. Copy of what the user sees; Edit
        // of a previously-edited paragraph then targets the visible
        // replacement rather than the untouched original.
        inlineTool.text = host.textContent ?? host.dataset.toolText ?? '';
      } else {
        inlineTool.key = null;
      }
    });
  }

  function onDocMouseDown(ev) {
    if (ev.target?.closest?.(TOOLBAR_SEL)) return;
    selTool.visible = false;
    inlineTool.key = null;
  }

  $effect(() => {
    document.addEventListener('mouseup', onDocMouseUp);
    document.addEventListener('mousedown', onDocMouseDown, true);
    return () => {
      document.removeEventListener('mouseup', onDocMouseUp);
      document.removeEventListener('mousedown', onDocMouseDown, true);
    };
  });

  async function selToolCopy() {
    try {
      await navigator.clipboard.writeText(selTool.text);
    } catch (e) {
      err = `copy failed: ${e.message}`;
    }
    selTool.visible = false;
    window.getSelection?.()?.removeAllRanges();
  }

  function selToolEdit() {
    const sel = window.getSelection?.();
    if (!sel || sel.rangeCount === 0) { selTool.visible = false; return; }
    const range = sel.getRangeAt(0);
    const startEl = range.startContainer?.nodeType === 3
      ? range.startContainer.parentElement
      : range.startContainer;
    const host = startEl?.closest?.('.paragraph-text, .clip-cell-text');
    if (!host) { selTool.visible = false; return; }
    // Derive `before` / `after` from DOM ranges over the rendered
    // host — necessary when the host contains prior hand-edited spans
    // whose text differs from the raw `p.text` transcription. The
    // excerpt itself comes from the selection's own toString().
    const beforeRange = document.createRange();
    beforeRange.selectNodeContents(host);
    try { beforeRange.setEnd(range.startContainer, range.startOffset); }
    catch { selTool.visible = false; return; }
    const before = beforeRange.toString();
    const afterRange = document.createRange();
    afterRange.selectNodeContents(host);
    try { afterRange.setStart(range.endContainer, range.endOffset); }
    catch { selTool.visible = false; return; }
    const after = afterRange.toString();
    const excerpt = sel.toString();
    if (!excerpt) { selTool.visible = false; return; }
    const paragraphId = host.classList.contains('paragraph-text')
      ? host.dataset.paragraphId
      : host.closest('.clip-cell-row')?.dataset.paragraphId;
    if (!paragraphId) { selTool.visible = false; return; }
    openEditor({
      hostKey: host.dataset.toolKey,
      paragraphId,
      before,
      excerpt,
      after,
    });
    selTool.visible = false;
    window.getSelection?.()?.removeAllRanges();
  }

  async function inlineToolCopy() {
    const key = inlineTool.key;
    try {
      await navigator.clipboard.writeText(inlineTool.text);
      flashKey = key;
      if (flashClearTimer) clearTimeout(flashClearTimer);
      flashClearTimer = setTimeout(() => {
        flashKey = null;
        flashClearTimer = 0;
      }, FLASH_MS);
    } catch (e) {
      err = `copy failed: ${e.message}`;
    }
    inlineTool.key = null;
  }

  function inlineToolEdit() {
    const key = inlineTool.key;
    const excerpt = inlineTool.text;
    if (!key || !excerpt) { inlineTool.key = null; return; }
    // Look up the paragraph id from whichever host currently carries
    // this tool-key. `.paragraph-text` has data-paragraph-id directly;
    // `.clip-cell-text` sits inside a row that does.
    const host = document.querySelector(`[data-tool-key="${cssEscape(key)}"]`);
    const paragraphId = host?.classList?.contains('paragraph-text')
      ? host.dataset.paragraphId
      : host?.closest?.('.clip-cell-row')?.dataset.paragraphId;
    if (!paragraphId) { inlineTool.key = null; return; }
    openEditor({
      hostKey: key,
      paragraphId,
      before: '',
      excerpt,
      after: '',
    });
    inlineTool.key = null;
  }

  // CSS.escape is widely available; the fallback covers the id/uuid
  // characters our tool keys actually use.
  function cssEscape(s) {
    if (typeof CSS !== 'undefined' && typeof CSS.escape === 'function') return CSS.escape(s);
    return String(s).replace(/["\\]/g, '\\$&');
  }

  // Inline text editor state. `hostKey` matches the `data-tool-key` of
  // the paragraph-text / clip-cell-text host and drives which host
  // renders the textarea in place of the excerpt. `before` + `excerpt`
  // + `after` reproduce the original host text; on Apply we POST the
  // (excerpt → draft) pair as a `ParagraphEdit` patch and let the
  // display path substring-replace it back in with a green underline.
  let editing = $state({
    hostKey: null,
    paragraphId: null,
    before: '',
    excerpt: '',
    after: '',
    draft: '',
    saving: false,
  });
  let editingTextareaEl = $state(null);

  async function openEditor(seed) {
    editing.hostKey = seed.hostKey;
    editing.paragraphId = seed.paragraphId;
    editing.before = seed.before;
    editing.excerpt = seed.excerpt;
    editing.after = seed.after;
    editing.draft = seed.excerpt;
    editing.saving = false;
    await tick();
    autoSizeEditor();
    editingTextareaEl?.focus();
    editingTextareaEl?.select();
  }

  // Grow the textarea to fit its current content. Called on open so
  // the initial size mirrors the excerpt (a one-liner opens compact,
  // a paragraph opens tall), and on every input so the field expands
  // as the user types. `height=auto` first resets any prior inline
  // height so scrollHeight reads a truthful measurement.
  function autoSizeEditor() {
    const el = editingTextareaEl;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  }

  function resetEditing() {
    editing.hostKey = null;
    editing.paragraphId = null;
    editing.before = '';
    editing.excerpt = '';
    editing.after = '';
    editing.draft = '';
    editing.saving = false;
  }

  async function commitEdit() {
    if (!editing.paragraphId || editing.saving) return;
    const original = editing.excerpt;
    const replacement = editing.draft;
    if (replacement === original || replacement.trim() === '') {
      resetEditing();
      return;
    }
    editing.saving = true;
    const paragraphId = editing.paragraphId;
    try {
      const savedId =
        selected !== 'live' && selected !== 'active' ? selectedSaved?.id ?? null : null;
      if (savedId) {
        await api.addSavedParagraphEdit(savedId, paragraphId, original, replacement);
      } else {
        await api.addLiveParagraphEdit(paragraphId, original, replacement);
      }
      // Pull the fresh state so the new edit + its green underline
      // land before the editor closes — avoids a flash of unedited
      // text between the request completing and the next poll tick.
      await refresh().catch(() => {});
    } catch (e) {
      err = `edit failed: ${e.message}`;
    } finally {
      resetEditing();
    }
  }

  function cancelEdit() {
    resetEditing();
  }

  function onEditKey(e) {
    if (e.key === 'Enter' && !e.shiftKey && !e.ctrlKey && !e.metaKey) {
      e.preventDefault();
      commitEdit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelEdit();
    }
  }

  // Apply a paragraph's edit overlay to a piece of text (the paragraph
  // text or one of its clips' texts) and return a run of `[text,
  // edited]` segments. Each edit is a first-occurrence substring swap
  // of `original` → `replacement` applied sequentially to a running
  // "current text" string with a parallel `edited` bitmap. Sequential
  // application (rather than treating edits as independent patches on
  // the original transcription) is what lets a follow-up edit target
  // a previous replacement's text: at the moment we search for the
  // new edit's `original`, that replacement is already sitting in
  // `cur`. Multi-segment spans (a selection that straddled a prior
  // replacement boundary) also just work because we search the flat
  // string, not a segments list.
  //
  // Edits whose `original` is no longer present in `cur` (an LLM
  // rewrite drifted the text away) are silently orphaned.
  function editedSegments(text, edits) {
    let cur = text ?? '';
    let marks = new Uint8Array(cur.length);
    for (const edit of edits ?? []) {
      const original = edit?.original ?? '';
      if (!original) continue;
      const idx = cur.indexOf(original);
      if (idx === -1) continue;
      const replacement = edit.replacement ?? '';
      const beforeLen = idx;
      const afterStart = idx + original.length;
      const newMarks = new Uint8Array(beforeLen + replacement.length + (cur.length - afterStart));
      newMarks.set(marks.subarray(0, beforeLen), 0);
      newMarks.fill(1, beforeLen, beforeLen + replacement.length);
      newMarks.set(marks.subarray(afterStart), beforeLen + replacement.length);
      cur = cur.slice(0, idx) + replacement + cur.slice(afterStart);
      marks = newMarks;
    }
    const segs = [];
    let i = 0;
    while (i < cur.length) {
      let j = i;
      const m = marks[i];
      while (j < cur.length && marks[j] === m) j++;
      segs.push({ text: cur.slice(i, j), edited: m === 1 });
      i = j;
    }
    if (segs.length === 0) segs.push({ text: '', edited: false });
    return segs;
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
  const HARDEN_TIMEOUT_MS = 2000;
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
            <div class="clip-fill" style="width: {playbackDurationMs ? Math.min(1, playingTimeMs / playbackDurationMs) * 100 : 0}%" aria-hidden="true"></div>
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

{#snippet displayText(text, edits)}
  {#each editedSegments(text, edits) as seg}
    {#if seg.edited}<span class="hand-edited">{seg.text}</span>{:else}{seg.text}{/if}
  {/each}
{/snippet}

{#snippet paragraphEditor()}
  <span class="paragraph-editor" contenteditable="false">
    <textarea
      bind:this={editingTextareaEl}
      bind:value={editing.draft}
      onkeydown={onEditKey}
      oninput={autoSizeEditor}
      class="edit-field"
      rows="1"
      disabled={editing.saving}
      aria-label="Edit excerpt"
    ></textarea>
    <span class="edit-actions">
      <button
        type="button"
        class="sel-btn edit-apply"
        title="Apply edit (Enter)"
        aria-label="Apply edit"
        onclick={commitEdit}
        disabled={editing.saving}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <polyline points="4 12 10 18 20 6"></polyline>
        </svg>
      </button>
      <button
        type="button"
        class="sel-btn edit-cancel"
        title="Cancel edit (Esc)"
        aria-label="Cancel edit"
        onclick={cancelEdit}
        disabled={editing.saving}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M6 6l12 12M6 18L18 6"></path>
        </svg>
      </button>
    </span>
  </span>
{/snippet}

{#snippet inlineToolbar()}
  <span
    class="inline-selection-toolbar"
    role="toolbar"
    aria-label="Text actions"
    contenteditable="false"
  >
    <button
      type="button"
      class="sel-btn"
      title="Copy text"
      aria-label="Copy text"
      onclick={inlineToolCopy}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
      </svg>
    </button>
    <button
      type="button"
      class="sel-btn"
      title="Edit text"
      aria-label="Edit text"
      onclick={inlineToolEdit}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"></path>
        <path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"></path>
      </svg>
    </button>
  </span>
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
      <button
        type="button"
        class="paragraph-ts"
        class:copied={copiedTsId === p.id}
        title={fmtTime(p.created_at)}
        onclick={() => copyTimestamp(p.id, fmtTime(p.created_at))}
      >
        {copiedTsId === p.id ? 'copied' : fmtWallTimeRel(p.start_wall_ms, nowMs)}
        {#if copiedTsId === p.id}
          <span class="ts-full">{fmtTime(p.created_at)}</span>
        {/if}
      </button>
    </div>
    <div
      class="paragraph-text"
      data-paragraph-id={p.id}
      data-tool-key="p:{p.id}"
      data-tool-text={p.text || ''}
      class:flash-copied={flashKey === `p:${p.id}`}
      class:editing={editing.hostKey === `p:${p.id}`}
    >{#if editing.hostKey === `p:${p.id}`}{editing.before}{@render paragraphEditor()}{editing.after}{:else}{@render displayText(p.text || '', p.edits)}{#if inlineTool.key === `p:${p.id}`}{@render inlineToolbar()}{/if}{/if}</div>
    {@render clipStrip(p, recId)}
  </div>
{/snippet}

{#snippet clipCell(entry, recId)}
  <!-- One clip = one row in a split cluster. The paragraph head only
       renders on the first clip of a paragraph; subsequent clips are
       "continuation" cells that visually group via a shared left rule.
       The clip's *own* text is shown (not the joined paragraph text) so
       an overlapping utterance on another channel can slot in at the
       right vertical position for the specific clip it overlaps. -->
  {@const { p, clip, isFirstOfP, isLastOfP } = entry}
  {@const ch = p.channel || channelFallback}
  {@const isTts = ch === 'TTS'}
  {@const title = isTts ? (p.speaker || 'TTS') : (p.speaker || ch)}
  {@const channelType = isTts ? 'TTS' : 'VOX'}
  {@const pstate = paragraphState(p)}
  {@const pulsing = paragraphInflight(p)}
  {@const harden_secs = hardenCountdownSec(p)}
  {@const isTtsClip = !!(clip && clip.audio_url)}
  {@const playable = clip && (recId != null || isTtsClip)}
  <div
    class="clip-cell-block"
    class:first-of-paragraph={isFirstOfP}
    class:last-of-paragraph={isLastOfP}
    class:tts={isTts}
    class:playing={currentPlayingParagraphId === p.id}
    class:provisional={!p.hardened && clip && clip.provisional}
  >
    {#if isFirstOfP}
      <div class="paragraph-head clip-cell-head">
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
      </div>
    {/if}
    {#if clip}
      <div class="clip-cell-row" data-paragraph-id={p.id}>
        {#if playable}
          <button
            type="button"
            class="clip-cell-play"
            class:playing={playingClipId === clip.id}
            class:provisional={clip.provisional}
            class:tts={isTtsClip}
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
              <div class="clip-fill" style="width: {playbackDurationMs ? Math.min(1, playingTimeMs / playbackDurationMs) * 100 : 0}%" aria-hidden="true"></div>
            {/if}
            <span class="clip-cell-play-glyph">▸</span>
          </button>
        {:else}
          <span class="clip-cell-tick" aria-hidden="true">·</span>
        {/if}
        <div
          class="clip-cell-text"
          class:provisional={clip.provisional}
          data-tool-key="clip:{clip.id}"
          data-tool-text={clip.text || ''}
          data-paragraph-id={p.id}
          class:flash-copied={flashKey === `clip:${clip.id}`}
          class:editing={editing.hostKey === `clip:${clip.id}`}
        >{#if editing.hostKey === `clip:${clip.id}`}{editing.before}{@render paragraphEditor()}{editing.after}{:else}{@render displayText(clip.text || '', p.edits)}{#if inlineTool.key === `clip:${clip.id}`}{@render inlineToolbar()}{/if}{/if}</div>
        <button
          type="button"
          class="clip-cell-ts"
          class:copied={copiedTsId === clip.id}
          title={fmtTime(p.created_at)}
          onclick={() => copyTimestamp(clip.id, fmtTime(p.created_at))}
        >
          {copiedTsId === clip.id ? 'copied' : fmtWallTimeRel(clip.start_wall_ms ?? p.start_wall_ms, nowMs)}
          {#if copiedTsId === clip.id}
            <span class="ts-full">{fmtTime(p.created_at)}</span>
          {/if}
        </button>
      </div>
    {/if}
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
                silence · {fmtRelMs(row.wallEnd - row.wallStart)}
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
            {@const hasMasterBtn = recId != null && row.audioStart != null}
            {@const clipRowStart = hasMasterBtn ? 2 : 1}
            {@const totalRows = (hasMasterBtn ? 1 : 0) + row.numSlots}
            <div class="cluster split" style="grid-template-columns: repeat({row.channels.length}, 1fr)">
              {#each Array.from({ length: Math.max(0, row.channels.length - 1) }) as _, dividerIdx}
                <div
                  class="column-divider"
                  style="grid-column: {dividerIdx + 1}; grid-row: 1 / span {totalRows};"
                  aria-hidden="true"
                ></div>
              {/each}
              {#if hasMasterBtn}
                <button
                  type="button"
                  class="master-btn row-master row-master-split"
                  class:playing={isRowMasterPlaying(row, rows)}
                  onclick={() => playMasterFromRow(row, rows, recId)}
                  title="Play the recording from here"
                  style="grid-column: 1 / -1; grid-row: 1;"
                >
                  {#if isRowMasterPlaying(row, rows)}
                    <div class="clip-fill" style="width: {rowMasterProgress(row, rows) * 100}%" aria-hidden="true"></div>
                  {/if}
                  <span class="master-glyph">▶</span>
                </button>
              {/if}
              {#each row.orderedClips as entry (entry.clip ? entry.clip.id : entry.p.id)}
                <div
                  class="cluster-cell"
                  style="grid-column: {entry.colIndex + 1}; grid-row: {clipRowStart + entry.slotIdx - 1};"
                >
                  {#if entry.isInFlight}
                    {@render paragraphBlock(entry.p, recId)}
                  {:else}
                    {@render clipCell(entry, recId)}
                  {/if}
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

{#if selTool.visible}
  <div
    class="selection-toolbar"
    style="left: {selTool.x}px; top: {selTool.y}px;"
    role="toolbar"
    aria-label="Selection actions"
  >
    <button
      type="button"
      class="sel-btn"
      title="Copy selection"
      aria-label="Copy selection"
      onclick={selToolCopy}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="9" y="9" width="13" height="13" rx="2" ry="2"></rect>
        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>
      </svg>
    </button>
    <button
      type="button"
      class="sel-btn"
      title="Edit selection"
      aria-label="Edit selection"
      onclick={selToolEdit}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"></path>
        <path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"></path>
      </svg>
    </button>
  </div>
{/if}

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
  @media (max-width: 1280px) {
    .pane-head { padding-left: 44px; }
  }
  .pane-head h1 { font-size: 16px; margin: 0; line-height: 1.2; }

  /* Saved-recording variant. */
  .pane-head.saved {
    align-items: center;
    flex-wrap: nowrap;
    padding: 6px 4px;
  }
  @media (max-width: 1280px) {
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
  @media (max-width: 1280px) {
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
    row-gap: 2px;
    column-gap: 10px;
    align-items: start;
  }
  /* Each paragraph in the split cluster gets a distinct grid row so
     scanning top-to-bottom follows wall-clock start-time order across
     channels. Note: this does not proportionally align brief utterances
     inside a longer paragraph's rendered time span — that would require
     per-clip cells (see the message thread in git for context). */
  .cluster-cell {
    min-width: 0;
    padding: 0 4px;
  }
  /* Full-height vertical guide between channel columns. Rendered as
     a grid item spanning every row of the cluster so the dashed rule
     is continuous regardless of which rows actually have content in
     the adjacent column. */
  .column-divider {
    border-right: 1px dashed rgba(255,255,255,0.06);
    pointer-events: none;
    align-self: stretch;
    justify-self: end;
    width: 0;
  }
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

  /* ---- Per-clip cell (split cluster only) ----
     Each clip of a paragraph becomes its own row in the grid so that a
     brief utterance on another channel can occupy the row that matches
     its wall-clock time — sitting between two clips of an overlapping
     longer paragraph rather than pushed below the whole paragraph.
     Consecutive clips of the same paragraph are visually linked by a
     shared left rule. */
  .clip-cell-block {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 2px 6px 2px 8px;
    border-left: 2px solid rgba(122,162,255,0.28);
    border-radius: 0 4px 4px 0;
    min-width: 0;
  }
  .clip-cell-block.tts {
    border-left-color: rgba(255,204,102,0.4);
  }
  .clip-cell-block.first-of-paragraph {
    padding-top: 6px;
    margin-top: 4px;
  }
  .clip-cell-block.last-of-paragraph {
    padding-bottom: 4px;
    margin-bottom: 4px;
  }
  .clip-cell-block.playing {
    background: rgba(122,162,255,0.06);
  }
  .clip-cell-block.provisional {
    opacity: 0.78;
  }
  .clip-cell-block.provisional .clip-cell-text {
    font-style: italic;
  }
  .clip-cell-head {
    margin-bottom: 2px;
  }
  .clip-cell-row {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  .clip-cell-play {
    position: relative;
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 22px;
    height: 20px;
    padding: 0 6px;
    background: rgba(122,162,255,0.08);
    color: var(--accent);
    border: 1px solid rgba(122,162,255,0.3);
    border-radius: 999px;
    font: inherit;
    font-size: 11px;
    line-height: 1;
    cursor: pointer;
    overflow: hidden;
  }
  .clip-cell-play:hover:not(:disabled) {
    background: rgba(122,162,255,0.16);
    color: var(--text);
  }
  .clip-cell-play.tts {
    background: rgba(255,204,102,0.08);
    color: #ffcc66;
    border-color: rgba(255,204,102,0.4);
  }
  .clip-cell-play.tts:hover:not(:disabled) {
    background: rgba(255,204,102,0.16);
  }
  .clip-cell-play.playing {
    border-color: rgba(122,162,255,0.75);
    box-shadow: 0 0 0 1px rgba(122,162,255,0.35);
  }
  .clip-cell-play.provisional {
    font-style: italic;
    opacity: 0.7;
  }
  .clip-cell-play-glyph {
    position: relative;
    z-index: 1;
  }
  .clip-cell-tick {
    flex: 0 0 auto;
    display: inline-block;
    width: 22px;
    text-align: center;
    color: var(--muted);
    opacity: 0.55;
  }
  .clip-cell-text {
    flex: 1 1 auto;
    min-width: 0;
    color: var(--text);
    padding: 2px 6px;
    font-size: 14px;
    line-height: 1.4;
    text-align: left;
    word-break: break-word;
    white-space: normal;
    user-select: text;
  }
  .clip-cell-ts {
    flex: 0 0 auto;
    color: var(--muted);
    font-size: 10px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    background: transparent;
    border: 0;
    padding: 0;
    font-family: inherit;
    cursor: pointer;
    position: relative;
  }
  .clip-cell-ts:hover { color: var(--text); }
  .clip-cell-ts.copied { color: rgb(90, 220, 120); }

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
    background: transparent;
    border: 0;
    padding: 0;
    font-family: inherit;
    cursor: pointer;
    position: relative;
  }
  .paragraph-ts:hover { color: var(--text); }
  .paragraph-ts.copied { color: rgb(90, 220, 120); }
  /* Full timestamp shown just below the "copied" flash. Absolutely
     positioned so the row height doesn't jitter during the 700ms
     flash window. */
  .paragraph-ts .ts-full,
  .clip-cell-ts .ts-full {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 2px;
    font-size: 10px;
    color: var(--muted);
    white-space: nowrap;
    pointer-events: none;
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
    color: var(--text);
    padding: 4px 6px;
    font-size: 14px;
    line-height: 1.45;
    text-align: left;
    border-radius: 4px;
    min-width: 0;
    word-break: break-word;
    white-space: normal;
    user-select: text;
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

  /* Floating toolbar shown next to the cursor after a mouseup that
     ends a text selection inside a record-log paragraph or clip. */
  .selection-toolbar {
    position: fixed;
    z-index: 1000;
    display: inline-flex;
    gap: 2px;
    padding: 3px;
    background: rgba(20, 22, 28, 0.96);
    border: 1px solid rgba(255,255,255,0.14);
    border-radius: 6px;
    box-shadow: 0 4px 12px rgba(0,0,0,0.5);
    user-select: none;
  }
  .sel-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    background: transparent;
    color: var(--text);
    border: 0;
    border-radius: 4px;
    cursor: pointer;
    padding: 0;
  }
  .sel-btn:hover {
    background: rgba(122,162,255,0.16);
    color: var(--accent);
  }

  /* Inline variant shown at the end of a paragraph or clip text when
     the user mouseups without a text selection. Flows inline so it
     sits at the end of the last text line and wraps under the
     paragraph when there is no room. */
  .inline-selection-toolbar {
    display: inline-flex;
    gap: 2px;
    padding: 2px;
    margin-left: 6px;
    vertical-align: middle;
    background: rgba(20, 22, 28, 0.9);
    border: 1px solid rgba(255,255,255,0.12);
    border-radius: 5px;
    user-select: none;
  }
  .inline-selection-toolbar .sel-btn {
    width: 20px;
    height: 20px;
  }

  /* Copy-confirmation flash for the whole-paragraph / whole-clip copy
     path. The `flash-copied` class is applied for FLASH_MS ms so the
     user sees the copy landed. */
  @keyframes flash-copied-anim {
    0%   { background: rgba(46, 204, 74, 0.28); }
    100% { background: transparent; }
  }
  .paragraph-text.flash-copied,
  .clip-cell-text.flash-copied {
    animation: flash-copied-anim 0.7s ease-out;
  }

  /* Inline text editor. The excerpt gets extracted out of the flow
     and replaced by a `<span class="paragraph-editor">` that carries
     the textarea + Apply/Cancel buttons. `display: block` so the
     editor always breaks to its own line inside the paragraph text —
     matches the "interstitial line-breaking" behavior requested for
     the edit affordance. */
  .paragraph-editor {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    margin: 4px 0;
    user-select: text;
  }
  .paragraph-text.editing,
  .clip-cell-text.editing {
    border: 1px solid rgba(122,162,255,0.35);
    padding: 4px 6px;
    border-radius: 4px;
  }
  .edit-field {
    flex: 1 1 240px;
    min-width: 0;
    padding: 4px 6px;
    background: rgba(20, 22, 28, 0.9);
    color: var(--text);
    border: 1px solid rgba(255,255,255,0.14);
    border-radius: 4px;
    font: inherit;
    font-size: 14px;
    line-height: 1.4;
    resize: vertical;
    overflow: hidden;
    box-sizing: border-box;
  }
  .edit-field:focus {
    outline: none;
    border-color: rgba(122,162,255,0.65);
    box-shadow: 0 0 0 1px rgba(122,162,255,0.35);
  }
  .edit-actions {
    display: inline-flex;
    gap: 2px;
  }
  .edit-apply {
    color: rgb(90, 220, 120);
  }
  .edit-apply:hover:not(:disabled) {
    background: rgba(90, 220, 120, 0.16);
    color: rgb(120, 240, 150);
  }
  .edit-cancel {
    color: rgb(230, 120, 120);
  }
  .edit-cancel:hover:not(:disabled) {
    background: rgba(230, 120, 120, 0.16);
    color: rgb(250, 150, 150);
  }
  .sel-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Hand-edited slice of a paragraph or clip text. The underline
     signals that this run originated from a user correction rather
     than the transcription. */
  .hand-edited {
    border-bottom: 1.5px solid rgb(90, 220, 120);
    padding-bottom: 0;
  }
</style>
