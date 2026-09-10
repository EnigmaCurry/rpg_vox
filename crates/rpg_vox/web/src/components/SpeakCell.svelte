<script>
  // A single "design a speech clip" cell. The clip is rendered on the server,
  // cached, and — when played — pushed straight into the pipewire virtual mic
  // so Discord (or any other consumer patched into the source) hears it.
  // Playback never touches the browser's audio device: the whole point of
  // this app is to speak into Discord, so keeping one output path avoids
  // "why can't Discord hear me" surprises.
  //
  // States:
  //   empty     — just the + button
  //   editing   — form is unlocked; play synthesizes
  //   rendering — synthesis in flight; textareas + play locked; guessed bar
  //   playing   — server is pushing the cached clip through pipewire; the
  //               deterministic progress bar is driven from wall-clock
  //               elapsed vs the known clip durationMs
  //   rendered  — locked textareas; play replays cached clip; double-click
  //               a textarea to edit; if edited, X to cancel back to rendered

  import { onDestroy, onMount, tick } from 'svelte';
  import {
    createWidget,
    updateWidget,
    updateWidgetText,
    deleteWidget,
    fetchWidget,
    playWidget,
    stopPlayback,
    startRecording,
    stopRecording,
    cancelRecording,
  } from '../lib/api.js';
  import { scenesState } from '../lib/scenes.svelte.js';
  import { estimateMs, recordSample } from '../lib/estimator.js';

  // When used inside a Lane, the parent owns the clip's identity and passes
  // in any prior text/instruct/widgetId so the cell hydrates from persisted
  // state. The parent also gets change + delete callbacks so it can mirror
  // updates into its store and remove the slot from the lane on delete.
  //
  // Standalone use (no props) still works — falls back to the original
  // pre-lane behavior with an empty starting state.
  let {
    initialText = '',
    initialProfileId = null,
    initialWidgetId = null,
    startEditing = false,
    onchange = null,
    ondelete = null,
    // Fired after a successful render or hydration so the parent (a Lane)
    // can gate scene-level Play/Render on which cells are playable. Payload
    // is `{ ready, durationMs }` or `null` when the cell becomes unplayable
    // (deleted, edited-and-not-yet-re-rendered).
    onaudio = null,
    // Called once on mount with a handle the parent can use to drive this
    // cell from outside — { playToEnd, stop, isReady, render, hasText } —
    // and once on unmount with `null` so the parent can forget the handle.
    // Enables scene-level playback to run each cell's own play + progress
    // bar in sequence, all pointing at the same pipewire mic.
    bindPlayer = null,
    // Fired with `true` whenever the cell enters an editing form (fresh
    // typing, or Edit-clicked on a rendered clip) and `false` when it leaves.
    // Lets the parent know a cell is holding unsaved input so scene-level
    // Play/Render buttons can gate on that.
    onedit = null,
    // Voice-profile payload for this cell's next render: an array of one or
    // more `{ speaker, language, instruct, pitchSemitones, timeRatio,
    // detuneCents, pan, gainDb, delayMs }` entries. The parent resolves
    // this from the character's selected profile; N>1 configs produce a
    // hive-mind mix on the server.
    configs = [],
    // Available voice profiles for this cell's character: `[{id, name}]`.
    // Rendered as a dropdown that picks which profile the server renders.
    // Empty list → dropdown is hidden.
    profiles = [],
    // Fired just before a user-initiated play kicks off (button click,
    // hover-play, retry-then-auto-play). NOT fired when scene playback
    // drives the cell via `playToEnd`. Lets the parent stop an in-flight
    // scene loop when the user clicks Play on any individual clip.
    onSoloPlayIntent = null,
  } = $props();

  // Starting state:
  //   - has a widgetId (persisted, previously rendered): 'rendered' with the
  //     `hydrating` flag on — the collapsed square shows text immediately
  //     and we fetch the cached WAV in onMount. Failure demotes to 'editing'.
  //   - has text but no widgetId (draft not yet rendered): 'editing' with the
  //     form pre-filled.
  //   - fresh (nothing): 'empty' (+ button), or 'editing' if the parent
  //     asked us to open with a blank form.
  // svelte-ignore state_referenced_locally
  let state = $state(
    initialWidgetId
      ? 'rendered'
      : (initialText || startEditing ? 'editing' : 'empty'),
  );

  // True while the mount-time fetch is in flight. Guards play/edit so the
  // user doesn't hit "play" before the server has confirmed the clip exists.
  // svelte-ignore state_referenced_locally
  let hydrating = $state(!!initialWidgetId);
  // svelte-ignore state_referenced_locally
  let text = $state(initialText);
  // svelte-ignore state_referenced_locally
  let profileId = $state(initialProfileId);
  let error = $state('');

  // Snapshot of what produced the current cached clip — used to detect
  // "dirty" edits and to revert on cancel. Seeded from initial props so a
  // hydrated cell knows its "clean" state matches the persisted server row.
  // svelte-ignore state_referenced_locally
  let renderedText = $state(initialWidgetId ? initialText : '');
  // svelte-ignore state_referenced_locally
  let renderedProfileId = $state(initialWidgetId ? initialProfileId : null);
  // Whether the server currently holds a playable clip for this widget id.
  // Set by the mount-time metadata fetch and by every successful render;
  // cleared on delete/reset. Playback goes through /widgets/:id/say (mic),
  // so we don't need the WAV bytes on the browser side.
  let ready = $state(false);
  let durationMs = $state(0);   // exact playback duration from server header
  let sampleRate = $state(0);

  // Server-assigned id, set on first successful render. Persists through
  // subsequent updates so PUT /widgets/{id} overwrites the same record.
  // Null until the widget has ever been rendered (pre-render state is
  // purely client-side and doesn't touch the store).
  // svelte-ignore state_referenced_locally
  let widgetId = $state(initialWidgetId);

  // Progress-bar animation state (both rendering + playing).
  let progress = $state(0);
  let rafId = 0;
  let startedAt = 0;
  let estimatedMs = 0;

  // Active vox recording session id (state='recording'). Null otherwise.
  // Set by startRecording; consumed by stopRecording/cancelRecording; also
  // cleared on destroy so a mid-recording cell teardown doesn't leak a
  // session on the server.
  let recordingSessionId = $state(null);
  // Wall-clock elapsed shown while recording, in ms. Driven by the same
  // rAF ticker as rendering/playing, so the "meta" line reads live.
  let recordingElapsedMs = $state(0);

  // AbortController for the in-flight /widgets/:id/say fetch, so
  // stopExternal() (or a delete) can cancel scene playback promptly.
  let playAbort = null;

  // AbortController for the in-flight render (POST/PUT /widgets). Cancel
  // discards the response so cancelRender() can revert the cell to its
  // pre-render state — the server keeps synthesizing, but the client
  // stops awaiting.
  let renderAbort = null;
  // Wall-clock elapsed since the current render started, in ms. Driven
  // by the same rAF ticker as `progress`, so the timer beneath the
  // cancel-render bolt reads live.
  let renderElapsedMs = $state(0);
  // Wall-clock elapsed since the current playback started, in ms. Fed
  // by tickPlay so the MM:SS chip on the stop overlay reads live.
  let playElapsedMs = $state(0);

  let textEl = $state(null);   // <textarea> for `text`, focused on entry

  // Delete button: two-tap confirm. `deleteConfirm=true` swaps the label to
  // "really?" and the second click actually resets the cell. Auto-reverts
  // after a few seconds so a stray click doesn't leave the label armed.
  let deleteConfirm = $state(false);
  let deleteTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  // Retry button on the collapsed hover overlay: same two-tap confirm as
  // delete so a stray click on the hover panel doesn't spend a TTS round
  // trip. Armed state auto-clears after DELETE_CONFIRM_MS.
  let retryConfirm = $state(false);
  let retryTimer = 0;

  function cancelRaf() {
    if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
  }
  onDestroy(() => {
    cancelRaf();
    if (playAbort) { try { playAbort.abort(); } catch {} playAbort = null; }
    if (renderAbort) { try { renderAbort.abort(); } catch {} renderAbort = null; }
    if (deleteTimer) clearTimeout(deleteTimer);
    // Fire-and-forget cancel for an in-flight recording so the server
    // doesn't hold onto a dangling session buffer if the cell tears down
    // mid-record (route change, parent lane unmount, etc.).
    if (recordingSessionId) {
      const sid = recordingSessionId;
      recordingSessionId = null;
      cancelRecording(sid).catch(() => {});
    }
  });

  // Rehydrate from the server if the parent handed us a widgetId. The row
  // already carries text/instruct via props (mirrored through the /state
  // blob the scene store persists), so all we need to confirm is that the
  // server still holds the clip and to grab its duration/sample-rate
  // metadata for the progress bar. A 404 means the row or WAV was lost —
  // drop the stale id and drop back to editing so the user can re-render.
  onMount(async () => {
    if (!initialWidgetId) return;
    try {
      const result = await fetchWidget(initialWidgetId);
      sampleRate = result.sampleRate || 0;
      durationMs = result.durationMs || 0;
      renderedText = text;
      renderedProfileId = profileId;
      ready = true;
      hydrating = false;
      onaudio?.({ ready: true, durationMs });
    } catch (e) {
      hydrating = false;
      if (e?.status === 404) {
        widgetId = null;
        state = 'editing';
      } else {
        state = 'editing';
        error = e?.message || 'failed to load clip';
      }
    }
  });

  // Delete lives on both the editing form and the collapsed hover overlay
  // (via the trash button). Disarm whenever the cell leaves those states so
  // it isn't still armed on the next visit.
  $effect(() => {
    if (state !== 'editing' && state !== 'rendered' && deleteConfirm) {
      deleteConfirm = false;
      if (deleteTimer) { clearTimeout(deleteTimer); deleteTimer = 0; }
    }
  });

  // Retry lives on the collapsed hover overlay; disarm as soon as the cell
  // leaves 'rendered' (started playing, edited, etc.) so the next visit
  // doesn't find the button armed.
  $effect(() => {
    if (state !== 'rendered' && retryConfirm) {
      retryConfirm = false;
      if (retryTimer) { clearTimeout(retryTimer); retryTimer = 0; }
    }
  });

  // Notify the parent whenever this cell enters/leaves the editing form so
  // scene-level Play/Render buttons can treat an open editor as "dirty".
  let lastEditEmit = null;
  $effect(() => {
    if (!onedit) return;
    const editing = state === 'editing';
    if (editing === lastEditEmit) return;
    lastEditEmit = editing;
    onedit(editing);
  });

  // Notify the parent (a Lane, in Scenes context) whenever the persistable
  // subset changes. Deduped on a stringified snapshot so we don't spam the
  // scenes store on unrelated reactivity ticks. Not fired in standalone
  // (no-parent) usage.
  let lastEmit = null;
  $effect(() => {
    if (!onchange) return;
    const snap = { text, profileId, widgetId };
    const key = JSON.stringify(snap);
    if (key === lastEmit) return;
    lastEmit = key;
    onchange(snap);
  });

  const hasClip = $derived(ready);
  const isDirty = $derived(
    hasClip && (text !== renderedText || profileId !== renderedProfileId)
  );
  const progressPct = $derived(`${(progress * 100).toFixed(1)}%`);

  async function beginEditing() {
    // Any playback in flight (this cell or another) should stop before we
    // drop the user into an edit form — hearing the old take while
    // rewriting the caption is disorienting. Fire-and-forget so a network
    // hiccup doesn't block entering the form.
    stopPlayback().catch(() => {});
    state = 'editing';
    error = '';
    // Textarea only mounts once state flips out of `empty`, so wait a tick
    // for Svelte to render it before focusing.
    await tick();
    textEl?.focus();
  }

  function cancelEdit() {
    text = renderedText;
    profileId = renderedProfileId;
    error = '';
    state = 'rendered';
  }

  // Persist text-only edits without touching the audio. Point of this is
  // to correct a recording's STT transcript — clicking "save" instead of
  // "render" keeps the recorded WAV as-is and just updates the caption.
  async function saveText() {
    if (!widgetId) return;
    const t = text.trim();
    error = '';
    try {
      await updateWidgetText(widgetId, t);
      text = t;
      renderedText = t;
      renderedProfileId = profileId;
      state = 'rendered';
    } catch (e) {
      error = e.message || 'save failed';
    }
  }

  function onRetryClick() {
    if (retryConfirm) {
      if (retryTimer) { clearTimeout(retryTimer); retryTimer = 0; }
      retryConfirm = false;
      render();
      return;
    }
    retryConfirm = true;
    if (retryTimer) clearTimeout(retryTimer);
    retryTimer = setTimeout(() => { retryConfirm = false; retryTimer = 0; }, DELETE_CONFIRM_MS);
  }

  async function onDeleteClick() {
    if (deleteConfirm) {
      if (deleteTimer) { clearTimeout(deleteTimer); deleteTimer = 0; }
      deleteConfirm = false;
      // Fire the backend delete first so a failure leaves the widget on
      // screen with an error, rather than reset locally but stranded in
      // the DB. If there's no id yet (never rendered), skip straight to
      // the local reset.
      if (widgetId) {
        try {
          await deleteWidget(widgetId);
        } catch (e) {
          error = `delete failed: ${e.message || 'unknown'}`;
          return;
        }
      }
      // In lane context the parent owns the slot; hand off and let it
      // remove us. Standalone use resets in-place back to the + button.
      if (ondelete) {
        ondelete();
      } else {
        resetToEmpty();
      }
      return;
    }
    deleteConfirm = true;
    if (deleteTimer) clearTimeout(deleteTimer);
    deleteTimer = setTimeout(() => { deleteConfirm = false; deleteTimer = 0; }, DELETE_CONFIRM_MS);
  }

  function resetToEmpty() {
    cancelRaf();
    if (playAbort) { try { playAbort.abort(); } catch {} playAbort = null; }
    if (renderAbort) { try { renderAbort.abort(); } catch {} renderAbort = null; }
    renderElapsedMs = 0;
    ready = false;
    text = '';
    profileId = null;
    renderedText = '';
    renderedProfileId = null;
    durationMs = 0;
    sampleRate = 0;
    widgetId = null;
    progress = 0;
    error = '';
    state = 'empty';
    onaudio?.(null);
  }

  // ---- Render (server round-trip) ------------------------------------------
  function tickRender() {
    const elapsed = performance.now() - startedAt;
    renderElapsedMs = elapsed;
    // Hold at 95% until the response actually lands, then snap to 100.
    progress = Math.min((elapsed / estimatedMs) * 0.95, 0.95);
    rafId = requestAnimationFrame(tickRender);
  }

  async function render({ autoplay = true } = {}) {
    const t = text.trim();
    if (!t) { error = 'text is empty'; return; }
    // Stop any in-flight playback (this cell or another) before kicking off
    // a fresh synth — the retry mini-button reaches this via onRetryClick,
    // and a dirty-edit render lands here from the play-button router. In
    // both cases the user is asking to replace the audio, so keep hearing
    // the previous take is confusing.
    stopPlayback().catch(() => {});
    error = '';
    state = 'rendering';
    progress = 0;
    renderElapsedMs = 0;
    startedAt = performance.now();
    // Estimator input: total prompt length across the profile's layers so a
    // hive-mind of many configs is billed proportional to its actual work.
    const instructChars = Array.isArray(configs)
      ? configs.reduce((n, c) => n + (typeof c?.instruct === 'string' ? c.instruct.length : 0), 0)
      : 0;
    estimatedMs = estimateMs(t.length + instructChars);
    rafId = requestAnimationFrame(tickRender);

    // Fresh AbortController so cancelRender() can drop this specific fetch
    // without touching future ones.
    const ac = new AbortController();
    renderAbort = ac;

    try {
      // First render → create (server assigns id). Subsequent renders →
      // update in place so the store overwrites the same row + WAV file.
      // If we were hydrated with a stale widgetId (server row deleted out
      // from under us — e.g. sqlite wiped), fall back to create so the
      // user's play action still produces a clip.
      // Voice layers come from the resolved character profile via `configs`.
      // Pass the currently-loaded project so the server applies its TTS
      // dictionary to `t` (pronunciation proxies) before synthesis.
      const projectId = scenesState.selectedProjectId ?? null;
      const opts = { signal: ac.signal, projectId };
      let result;
      try {
        result = widgetId
          ? await updateWidget(widgetId, t, configs, opts)
          : await createWidget(t, configs, opts);
      } catch (e) {
        if (e?.name === 'AbortError') throw e;
        if (widgetId && /no widget with id/i.test(e.message || '')) {
          widgetId = null;
          result = await createWidget(t, configs, opts);
        } else {
          throw e;
        }
      }
      cancelRaf();
      if (renderAbort === ac) renderAbort = null;
      const elapsedMs = performance.now() - startedAt;
      recordSample(t.length + instructChars, elapsedMs);

      if (result.id) widgetId = result.id;
      ready = true;
      sampleRate = result.sampleRate || 0;
      durationMs = result.durationMs || 0;
      renderedText = t;
      renderedProfileId = profileId;
      onaudio?.({ ready: true, durationMs });

      progress = 1;
      renderElapsedMs = 0;
      await tick();
      if (autoplay) {
        // Brief flash of the filled render bar before playback begins so the
        // user sees the render "complete" before the wall-clock progress
        // resets to zero for the playback bar.
        setTimeout(() => {
          if (state === 'rendering') play();
        }, 180);
      } else {
        // Bulk-render caller doesn't want auto-play. Settle into the collapsed
        // rendered state so the next cell's render starts from a stable UI.
        progress = 0;
        state = 'rendered';
      }
    } catch (e) {
      cancelRaf();
      if (renderAbort === ac) renderAbort = null;
      // AbortError from cancelRender(): the cancel handler already reverted
      // state + progress; just bail without surfacing an error.
      if (e?.name === 'AbortError') return;
      state = hasClip ? 'rendered' : 'editing';
      progress = 0;
      renderElapsedMs = 0;
      error = e.message || 'render failed';
    }
  }

  // User-initiated cancel of an in-flight render. Aborts the fetch (the
  // server keeps synthesizing, but we drop the response) and reverts to
  // whichever state the cell was in before render() flipped it — a prior
  // cached clip if one existed, or the editing form otherwise. Text and
  // profile inputs stay so the user can tweak and retry.
  function cancelRender() {
    if (state !== 'rendering' || !renderAbort) return;
    try { renderAbort.abort(); } catch {}
    renderAbort = null;
    cancelRaf();
    progress = 0;
    renderElapsedMs = 0;
    state = hasClip ? 'rendered' : 'editing';
  }

  // ---- Play (through the pipewire virtual mic) ----------------------------
  // Wall-clock progress: the server plays the cached clip at its known
  // duration, so we drive the bar from elapsed time here instead of asking
  // the server for progress. Overshoot is clamped at 1.0 so a slightly-off
  // durationMs doesn't leave the bar spinning past the end.
  function tickPlay() {
    if (state !== 'playing') { cancelRaf(); return; }
    const total = durationMs || 1;
    const elapsed = performance.now() - startedAt;
    // Clamp the visible timer to the clip's known duration so a slightly
    // long server-side drain doesn't push MM:SS past the end of the clip.
    playElapsedMs = Math.min(elapsed, total);
    progress = Math.min(elapsed / total, 1);
    rafId = requestAnimationFrame(tickPlay);
  }

  async function play({ fromScene = false } = {}) {
    if (!ready || !widgetId) return;
    // User-initiated plays (button click, hover-play, retry) tell the
    // parent to abort any in-flight scene playback so the clicked clip
    // isn't preempted a moment later by the scene loop's next iteration.
    // Scene-driven plays (playToEnd) skip this so the loop can continue.
    if (!fromScene) {
      try { onSoloPlayIntent?.(); } catch {}
    }
    error = '';
    state = 'playing';
    progress = 0;
    playElapsedMs = 0;
    startedAt = performance.now();
    // Fresh AbortController per play so stopExternal() can cancel the current
    // request without affecting future ones.
    const ac = new AbortController();
    playAbort = ac;
    rafId = requestAnimationFrame(tickPlay);
    try {
      await playWidget(widgetId, { signal: ac.signal });
    } catch (e) {
      // AbortError from stopExternal is expected; other errors surface.
      if (e?.name !== 'AbortError') {
        error = e.message || 'playback failed';
      }
    } finally {
      cancelRaf();
      progress = 0;
      playElapsedMs = 0;
      if (playAbort === ac) playAbort = null;
      // Only demote if we're still in the playing state; a delete/reset
      // during playback may have already moved us on.
      if (state === 'playing') state = 'rendered';
    }
  }

  // ---- Record (capture from the pipewire vox channel) ---------------------
  // The server subscribes to the `-vox` sink's PCM tap for the duration of
  // this session and encodes the accumulated samples as a mono 16-bit WAV
  // on stop — matching the widget clip format so playback, scene mix, and
  // rehydration all share code with TTS-rendered clips.
  function tickRecord() {
    if (state !== 'recording') { cancelRaf(); return; }
    recordingElapsedMs = performance.now() - startedAt;
    rafId = requestAnimationFrame(tickRecord);
  }

  async function startRecord() {
    error = '';
    // Trim the caption once here so the server persists the same string
    // we'll pin as `renderedText` on stop — otherwise trailing whitespace
    // would make the cell instantly look dirty after saving.
    const captionAtStart = text.trim();
    try {
      const { sessionId } = await startRecording({
        widgetId: widgetId,
        text: captionAtStart,
      });
      recordingSessionId = sessionId;
      recordingElapsedMs = 0;
      startedAt = performance.now();
      state = 'recording';
      rafId = requestAnimationFrame(tickRecord);
    } catch (e) {
      error = e.message || 'failed to start recording';
    }
  }

  async function stopRecord() {
    if (!recordingSessionId) return;
    const sid = recordingSessionId;
    recordingSessionId = null;
    cancelRaf();
    try {
      const result = await stopRecording(sid);
      widgetId = result.id;
      sampleRate = result.sampleRate || 0;
      durationMs = result.durationMs || 0;
      ready = true;
      // Prefer the server's transcript when present + non-empty; server
      // has already persisted whichever it chose as the widget's `text`
      // column, so mirror that here so renderedText/text stay in sync
      // and the cell doesn't look dirty right after saving. Fall back to
      // whatever the user typed (trimmed to match the server side).
      if (typeof result.transcript === 'string' && result.transcript.length > 0) {
        text = result.transcript;
      } else {
        text = text.trim();
      }
      renderedText = text;
      renderedProfileId = profileId;
      onaudio?.({ ready: true, durationMs });
      state = 'rendered';
      recordingElapsedMs = 0;
    } catch (e) {
      state = hasClip ? 'rendered' : 'editing';
      error = e.message || 'stop recording failed';
      recordingElapsedMs = 0;
    }
  }

  async function cancelRecord() {
    if (!recordingSessionId) return;
    const sid = recordingSessionId;
    recordingSessionId = null;
    cancelRaf();
    recordingElapsedMs = 0;
    state = hasClip ? 'rendered' : 'editing';
    try {
      await cancelRecording(sid);
    } catch (e) {
      // Cancel is best-effort; server-side session is dropped either way
      // when the sid is unknown, so surface at debug level only.
      console.debug('cancel recording failed', e);
    }
  }

  // ---- External driver (scene playback) -----------------------------------
  // Kick playback and resolve when the pipewire drain completes (natural end)
  // or when stopExternal() aborts the request. The parent can call `.stop()`
  // to interrupt without waiting for the promise.
  async function playToEnd() {
    if (!ready || !widgetId) return;
    await play({ fromScene: true });
  }

  function stopExternal() {
    // Two-step stop: abort the /say fetch so this client stops awaiting,
    // AND fire /playback/stop so the server flushes the pipewire ring
    // and interrupts the TTS runner. Without the second step, audio
    // already queued in the ring keeps playing for up to ringbuf_seconds.
    if (playAbort) { try { playAbort.abort(); } catch {} }
    stopPlayback().catch(() => {});
    cancelRaf();
    progress = 0;
    playElapsedMs = 0;
    if (state === 'playing') state = 'rendered';
  }

  onMount(() => {
    bindPlayer?.({
      playToEnd,
      stop: stopExternal,
      isReady: () => ready && !!widgetId,
      render,
      hasText: () => text.trim().length > 0,
    });
  });
  onDestroy(() => {
    bindPlayer?.(null);
  });

  // ---- Play button router --------------------------------------------------
  // Editing (no clip, or dirty edits): synthesize.
  // Rendered (locked, clean): replay cached clip through the mic.
  function onPlayClick() {
    if (state === 'editing') {
      if (hasClip && !isDirty) play();
      else                     render();
      return;
    }
    if (state === 'rendered') play();
  }

  function onTextKey(e) {
    // Ctrl/Cmd+Enter always triggers the primary action for the current state.
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      onPlayClick();
    }
  }

  const clipMeta = $derived(hasClip ? `${(durationMs / 1000).toFixed(1)}s` : '');

  // MM:SS for the render timer — long TTS jobs routinely run into minutes,
  // so seconds-only overflows past 60 and reads as "how long has this been
  // stuck?" better as a stopwatch than a raw decimal.
  function formatMMSS(ms) {
    const total = Math.max(0, Math.floor(ms / 1000));
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
  }
  const renderElapsedLabel = $derived(formatMMSS(renderElapsedMs));
  const playElapsedLabel = $derived(formatMMSS(playElapsedMs));

  // Dropdown value is always a string (even for the "default" empty selection),
  // so we normalize null ↔ '' when reading/writing the profileId state.
  const profileSelectValue = $derived(profileId ?? '');
  function onProfileSelectChange(ev) {
    const val = ev.currentTarget.value;
    profileId = val === '' ? null : val;
  }
  // Show the picker only if the parent handed us any profiles.
  const hasProfiles = $derived(Array.isArray(profiles) && profiles.length > 0);
</script>

<div class="cell"
     class:playing={state === 'playing' || state === 'rendering' || state === 'recording'}
     class:collapsed={state === 'rendered' || state === 'playing' || (state === 'rendering' && hasClip)}
     class:form-mode={state === 'editing' || (state === 'rendering' && !hasClip) || state === 'recording'}>
  {#if state === 'empty'}
    <button class="plus" onclick={beginEditing} aria-label="Add speak cell">+</button>

  {:else if state === 'recording'}
    <!-- Recording form: textarea stays visible (locked) so the caption typed
         beforehand is still shown, Stop replaces Play, Cancel discards the
         session. Progress is a wall-clock timer since we don't know a
         target length. -->
    <div class="form">
      <div class="form-left">
        <textarea
          class="text"
          bind:value={text}
          placeholder="Text to speak…"
          disabled></textarea>
      </div>

      <div class="side">
        <button
          class="play stop-record"
          onclick={stopRecord}
          aria-label="Stop recording"
          title="Stop recording and save clip">
          <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
            <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
          </svg>
        </button>
        <div class="meta">
          <span class="rec-dot" aria-hidden="true"></span>
          {(recordingElapsedMs / 1000).toFixed(1)}s
        </div>
        <button
          class="text-btn cancel"
          onclick={cancelRecord}
          title="Discard recording">
          cancel
        </button>
      </div>
    </div>

    {#if error}
      <div class="err">{error}</div>
    {/if}

  {:else if state === 'rendered' || state === 'playing' || (state === 'rendering' && hasClip)}
    <!-- Collapsed square: shows as much of the spoken text as fits at the
         current size. Font size scales with the container via cqmin so the
         readable-char-count changes with the widget's rendered size. -->
    <div class="clip-text">{text}</div>

    {#if state === 'playing'}
      <!-- Full-cell stop button so clicking anywhere on a playing clip
           cancels playback. Sits above .progress (z-index) and reuses
           stopExternal to abort the /widgets/:id/say fetch and demote to
           'rendered'. Stop icon fades in on hover so the affordance is
           discoverable but doesn't clutter the collapsed clip text. -->
      <button
        class="stop-overlay"
        onclick={stopExternal}
        aria-label="Stop playback"
        title="Stop playback">
        <svg class="stop-icon" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
        </svg>
        <!-- Live MM:SS elapsed. Own dim background chip so it stays legible
             over the clip text (which is visible at rest, since .stop-overlay
             only dims on hover). -->
        <div class="play-timer">{playElapsedLabel}</div>
      </button>
    {/if}

    {#if state === 'rendering'}
      <!-- Retry-triggered render (from the collapsed hover Retry button):
           the prior clip's text is still shown underneath. Bolt icon sits
           in a rounded-square underlay that matches .play-hover so both
           the icon and the MM:SS timer read cleanly regardless of what's
           printed beneath. Click cancels the fetch and restores the prior
           cached clip. -->
      <button
        class="cancel-render-overlay"
        onclick={cancelRender}
        aria-label="Cancel generation"
        title="Cancel generation">
        <span class="bolt-box">
          <svg class="bolt-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path fill="currentColor" d="M13 2L3 14h9l-1 8 10-12h-9l1-8z"/>
          </svg>
        </span>
        <div class="cancel-timer">{renderElapsedLabel}</div>
      </button>
    {/if}

    {#if state === 'rendered' && !hydrating}
      <div class="hover-actions">
        <button class="play-hover" onclick={play} aria-label="Play">
          <svg class="play-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path fill="currentColor" d="M8 5v14l11-7z" />
          </svg>
        </button>
        <div class="mini-row">
          <button class="mini" onclick={beginEditing} aria-label="Edit" title="Edit text / instruction">
            <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
              <path fill="currentColor" d="M3 17.25V21h3.75L17.81 9.94l-3.75-3.75L3 17.25zM20.71 7.04c.39-.39.39-1.02 0-1.41l-2.34-2.34a.996.996 0 0 0-1.41 0l-1.83 1.83 3.75 3.75 1.83-1.83z"/>
            </svg>
          </button>
          <button
            class="mini red"
            class:confirm={retryConfirm}
            onclick={onRetryClick}
            aria-label={retryConfirm ? 'Confirm re-render' : 'Re-render clip'}
            title={retryConfirm ? 'Click again to re-render' : 'Re-render clip'}
          >
            <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
              <path fill="currentColor" d="M17.65 6.35A7.958 7.958 0 0 0 12 4C7.58 4 4 7.58 4 12s3.58 8 8 8c3.73 0 6.84-2.55 7.73-6h-2.08A5.99 5.99 0 0 1 12 18c-3.31 0-6-2.69-6-6s2.69-6 6-6c1.66 0 3.14.69 4.22 1.78L13 11h7V4l-2.35 2.35z"/>
            </svg>
          </button>
          <button
            class="mini red"
            class:confirm={deleteConfirm}
            onclick={onDeleteClick}
            aria-label={deleteConfirm ? 'Confirm delete' : 'Delete clip'}
            title={deleteConfirm ? 'Click again to delete' : 'Delete clip'}
          >
            <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
              <path fill="currentColor" d="M9 3v1H4v2h1v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V6h1V4h-5V3H9zm2 5h2v9h-2V8zm-4 0h2v9H7V8zm8 0h2v9h-2V8z"/>
            </svg>
          </button>
        </div>
      </div>
    {/if}

    {#if hydrating}
      <div class="hydrating-veil" aria-hidden="true">
        <span class="spinner" aria-hidden="true"></span>
      </div>
    {/if}

    {#if state === 'playing' || state === 'rendering'}
      <div class="progress" style:width={progressPct}></div>
    {/if}

  {:else}
    <!-- editing / rendering: the form. -->
    <div class="form">
      <div class="form-left">
        <textarea
          class="text"
          bind:this={textEl}
          bind:value={text}
          onkeydown={onTextKey}
          placeholder="Text to speak…"
          disabled={state === 'rendering'}></textarea>

        {#if hasProfiles}
          <label class="profile-picker">
            <span>Profile</span>
            <select
              value={profileSelectValue}
              onchange={onProfileSelectChange}
              disabled={state === 'rendering'}
              aria-label="Voice profile"
            >
              <!-- Empty value = "use the character's default profile". Keeps
                   fresh clips silently on the default until the user opts in. -->
              <option value="">(default)</option>
              {#each profiles as p (p.id)}
                <option value={p.id}>{p.name}</option>
              {/each}
            </select>
          </label>
        {/if}
      </div>

      <div class="side">
        {#if state === 'rendering'}
          <!-- Rendering: replace Play with a Cancel-generation button
               (lightning bolt); wall-clock timer below shows how long
               the synth has been running. Clicking aborts the fetch and
               reverts the cell to its pre-render state. -->
          <button
            class="play cancel-render"
            onclick={cancelRender}
            aria-label="Cancel generation"
            title="Cancel generation">
            <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
              <path fill="currentColor" d="M13 2L3 14h9l-1 8 10-12h-9l1-8z"/>
            </svg>
          </button>
          <div class="meta timer">{renderElapsedLabel}</div>
        {:else}
          <button
            class="play"
            onclick={onPlayClick}
            aria-label={hasClip && !isDirty ? 'Play' : 'Render'}
            title={isDirty ? 'Re-render with new text' : hasClip ? 'Play cached clip' : 'Render'}>
            <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
              <path fill="currentColor" d="M8 5v14l11-7z" />
            </svg>
          </button>
        {/if}

        <button
          class="play record"
          onclick={startRecord}
          disabled={state === 'rendering'}
          aria-label="Record from vox channel"
          title="Record audio from the vox channel">
          <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
            <circle cx="12" cy="12" r="6" fill="currentColor" />
          </svg>
        </button>

        {#if clipMeta && state !== 'rendering'}
          <div class="meta">{clipMeta}</div>
        {/if}
      </div>
    </div>

    {#if state === 'rendering'}
      <div class="progress" style:width={progressPct}></div>
    {/if}

    {#if error && state !== 'rendering'}
      <div class="err">{error}</div>
    {/if}

    {#if state === 'editing'}
      <div class="cell-actions">
        {#if hasClip}
          <button
            class="text-btn save"
            onclick={saveText}
            title="Save edited text without re-rendering audio">
            save
          </button>
          <button
            class="text-btn cancel"
            onclick={cancelEdit}
            title="Discard edits, restore rendered clip">
            cancel
          </button>
          <button
            class="text-btn rerender"
            onclick={render}
            title="Save and re-render, even if the text hasn't changed">
            render
          </button>
        {/if}
        <button
          class="text-btn delete"
          class:confirm={deleteConfirm}
          onclick={onDeleteClick}
          title="Delete this cell">
          {deleteConfirm ? 'really?' : 'delete'}
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .cell {
    position: relative;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    min-height: 180px;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    /* Fixed width in both form and collapsed states so switching modes doesn't
       reflow the surrounding layout. Later, when cells live in a grid, this
       cap comes off and the grid track dictates size. */
    width: min(320px, 100%);
  }
  .cell.playing { border-color: var(--accent); }

  /* Collapsed square view (rendered + playing). aspect-ratio forces 1:1
     regardless of the width the parent grants. container-type lets .clip-text
     size its font relative to the container dimensions via cqmin. */
  .cell.collapsed {
    aspect-ratio: 1 / 1;
    min-height: 0;
    container-type: size;
    container-name: cell;
  }

  /* Form (editing/rendering) is at least as tall as the collapsed square so
     switching in and out of the form doesn't shrink the widget's footprint.
     Content taller than this pushes the cell larger naturally. */
  .cell.form-mode {
    min-height: min(320px, 100vw);
  }

  .clip-text {
    width: 100%;
    height: 100%;
    padding: 8cqmin;
    /* font-size grows with the container but clamps so the widget stays
       legible at tiny sizes and doesn't become absurd at huge ones. */
    font-size: clamp(12px, 8cqmin, 36px);
    line-height: 1.18;
    color: var(--text);
    word-break: break-word;
    overflow: hidden;
    /* Fade the bottom edge so clipped text visually implies "there's more". */
    -webkit-mask-image: linear-gradient(to bottom, black 82%, transparent 100%);
            mask-image: linear-gradient(to bottom, black 82%, transparent 100%);
    white-space: pre-wrap;
  }

  .hover-actions {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 10px;
    background: rgba(0, 0, 0, 0.55);
    opacity: 0;
    transition: opacity 0.12s ease-in-out;
    z-index: 3;
  }
  .cell.collapsed:hover .hover-actions,
  .cell.collapsed:focus-within .hover-actions { opacity: 1; }

  /* Full-cell click target that appears while a clip is playing. The stop
     icon is faint until hover so the .clip-text stays readable at rest,
     but the whole square is clickable throughout (the button covers it). */
  .stop-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    background: transparent;
    border: none;
    padding: 0;
    margin: 0;
    color: var(--accent);
    cursor: pointer;
    z-index: 3;
  }
  .stop-overlay:hover,
  .stop-overlay:focus-visible {
    background: rgba(0, 0, 0, 0.35);
    outline: none;
  }
  .stop-icon {
    width: 34%;
    height: 34%;
    opacity: 0.55;
    transition: opacity 0.12s ease-in-out;
  }
  .stop-overlay:hover .stop-icon,
  .stop-overlay:focus-visible .stop-icon { opacity: 1; }
  /* Own chip background so the timer stays legible over the clip text
     at rest — .stop-overlay itself is transparent unless hovered. */
  .play-timer {
    padding: 2px 8px;
    background: rgba(0, 0, 0, 0.55);
    color: var(--accent);
    font-size: clamp(11px, 4.5cqmin, 15px);
    font-variant-numeric: tabular-nums;
    border-radius: 4px;
    opacity: 0.9;
  }
  .stop-overlay:hover .play-timer,
  .stop-overlay:focus-visible .play-timer { opacity: 1; }

  /* Primary action: fills most of the cell as a large square target. Grows
     into whatever height flex leaves after the edit button + gap, then the
     aspect-ratio pin makes width match so it stays square regardless of
     cell size. */
  .play-hover {
    flex: 1 1 auto;
    aspect-ratio: 1 / 1;
    max-width: 100%;
    min-height: 0;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(255, 255, 255, 0.14);
    color: var(--accent);
    border: 1px solid rgba(255, 255, 255, 0.18);
    border-radius: 8px;
    cursor: pointer;
  }
  .play-hover:hover {
    background: rgba(255, 255, 255, 0.22);
    border-color: rgba(255, 255, 255, 0.28);
  }
  .play-icon {
    width: 45%;
    height: 45%;
  }

  /* Row of small round secondary actions pinned below the play square:
     edit, retry, trash. `.mini.red` variants (retry, trash) hover red so
     the destructive-ish actions read as distinct from a benign edit. */
  .mini-row {
    display: flex;
    gap: 6px;
    align-items: center;
    flex: 0 0 auto;
  }
  .mini {
    width: 30px;
    height: 30px;
    border-radius: 50%;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(255, 255, 255, 0.14);
    color: var(--text);
    border: 1px solid rgba(255, 255, 255, 0.18);
    cursor: pointer;
  }
  .mini:hover {
    background: rgba(255, 255, 255, 0.22);
    border-color: rgba(255, 255, 255, 0.28);
  }
  .mini.red:hover,
  .mini.red.confirm {
    background: rgba(255, 128, 128, 0.20);
    color: var(--err);
    border-color: var(--err);
  }

  .plus {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    color: var(--muted);
    border: none;
    font-size: 40px;
    font-weight: 300;
    cursor: pointer;
    min-height: 180px;
  }
  .plus:hover { color: var(--accent); background: rgba(122,162,255,0.04); }

  .form {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 10px;
    padding: 12px;
    position: relative;
    z-index: 1;
    /* Fill the cell's remaining vertical space (cell is flex-column) so the
       textareas can be percentage-sized against a real height. */
    flex: 1;
    min-height: 0;
  }

  /* Left column: text pane fills all available height; style picker sits
     under it as a short row. */
  .form-left {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    /* min-width:0 prevents flex/grid from forcing intrinsic width from the
       textarea, which would otherwise blow the outer grid column out. */
    min-width: 0;
  }
  .text {
    flex: 1;
    width: 100%;
    resize: none;
    margin: 0;
    font-size: 1.5em;
    min-height: 0;
  }

  .profile-picker {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .profile-picker select {
    flex: 1;
    min-width: 0;
    text-transform: none;
    letter-spacing: normal;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 6px;
    font-size: 12px;
    font-family: inherit;
    cursor: pointer;
  }
  .profile-picker select:focus { outline: none; border-color: var(--accent); }
  .profile-picker select:disabled { opacity: 0.6; cursor: not-allowed; }

  /* Disabled textareas (rendering state) must look non-interactive: muted
     text, dimmed background, no focus ring. `rendered` collapses the form
     entirely so we don't need a readonly variant. */
  textarea:disabled {
    color: var(--muted);
    background: rgba(0, 0, 0, 0.28);
    border-color: transparent;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.02);
    cursor: not-allowed;
    opacity: 0.75;
  }
  textarea:disabled:focus { outline: none; }

  .side {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    justify-content: flex-start;
  }
  .meta {
    font-size: 11px;
    color: var(--muted);
    text-align: center;
    max-width: 80px;
    line-height: 1.3;
  }

  .play {
    width: 44px;
    height: 44px;
    border-radius: 8px;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  /* Record button: same footprint as Play so the two stack cleanly, tinted
     red so it's obvious this path bypasses TTS and captures live audio. */
  .play.record {
    color: var(--err);
    background: rgba(255, 128, 128, 0.10);
    border-color: rgba(255, 128, 128, 0.35);
  }
  .play.record:hover:not(:disabled) {
    background: rgba(255, 128, 128, 0.20);
    border-color: var(--err);
  }
  .play.stop-record {
    color: var(--err);
    background: rgba(255, 128, 128, 0.20);
    border-color: var(--err);
  }

  /* Cancel-generation button in form mode: same footprint as Play/Record
     but tinted amber so it reads as "in progress, click to abort" rather
     than a normal action. */
  .play.cancel-render {
    color: #ffcf5a;
    background: rgba(255, 207, 90, 0.12);
    border-color: rgba(255, 207, 90, 0.45);
  }
  .play.cancel-render:hover {
    background: rgba(255, 207, 90, 0.22);
    border-color: #ffcf5a;
  }

  /* Full-cell cancel button that overlays the collapsed clip during a
     retry-triggered render. Persistent dim background so the bolt puck
     and timer read cleanly on top of the clip text underneath — same
     approach as .hover-actions. */
  .cancel-render-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 10px;
    background: rgba(0, 0, 0, 0.55);
    border: none;
    margin: 0;
    color: #ffcf5a;
    cursor: pointer;
    z-index: 3;
  }
  .cancel-render-overlay:hover,
  .cancel-render-overlay:focus-visible {
    background: rgba(0, 0, 0, 0.7);
    outline: none;
  }
  /* Mirrors .play-hover: a tinted rounded square holding the icon so the
     bolt reads as an actionable button on top of the dim overlay. */
  .bolt-box {
    flex: 0 1 auto;
    width: min(45%, 90px);
    aspect-ratio: 1 / 1;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(255, 255, 255, 0.14);
    border: 1px solid rgba(255, 255, 255, 0.22);
    border-radius: 8px;
  }
  .cancel-render-overlay:hover .bolt-box,
  .cancel-render-overlay:focus-visible .bolt-box {
    background: rgba(255, 255, 255, 0.22);
    border-color: rgba(255, 255, 255, 0.32);
  }
  .bolt-icon {
    width: 55%;
    height: 55%;
  }
  .cancel-timer {
    font-size: clamp(12px, 5cqmin, 18px);
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  /* Form-mode timer under the side-panel bolt: match the bolt tint so the
     stopwatch clearly belongs with the cancel affordance above it. */
  .meta.timer {
    color: #ffcf5a;
    font-variant-numeric: tabular-nums;
  }

  /* Pulsing red dot beside the elapsed-time meta line while a recording is
     in flight — familiar "REC" affordance. */
  .rec-dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--err);
    margin-right: 4px;
    vertical-align: middle;
    animation: rec-blink 1.1s ease-in-out infinite;
  }
  @keyframes rec-blink {
    0%, 100% { opacity: 1; }
    50%      { opacity: 0.35; }
  }


  .spinner {
    width: 16px;
    height: 16px;
    border: 2px solid rgba(0,0,0,0.25);
    border-top-color: #0b0d10;
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .progress {
    position: absolute;
    inset: 0 auto 0 0;
    background: rgba(122,162,255,0.12);
    pointer-events: none;
    transition: width 0.08s linear;
    z-index: 0;
  }

  /* Subtle overlay while the mount-time fetch is in flight. Sits above the
     .clip-text but below .hover-actions (which are hidden during hydrate),
     so the user sees the text they typed with a small spinner in the corner. */
  .hydrating-veil {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: flex-end;
    justify-content: flex-end;
    padding: 8px;
    background: rgba(0, 0, 0, 0.15);
    pointer-events: none;
    z-index: 2;
  }
  .hydrating-veil .spinner {
    border-color: rgba(255,255,255,0.25);
    border-top-color: var(--accent);
  }

  .err {
    padding: 6px 12px 10px;
    color: var(--err);
    font-size: 12px;
  }

  .cell-actions {
    position: absolute;
    right: 8px;
    bottom: 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-items: flex-end;
    z-index: 2;
  }

  .text-btn {
    padding: 3px 10px;
    font-size: 11px;
    font-weight: 500;
    color: var(--muted);
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
    font-family: inherit;
  }
  .text-btn.save:hover     { color: var(--accent); border-color: var(--accent); }
  .text-btn.cancel:hover   { color: var(--text);   border-color: var(--muted); }
  .text-btn.rerender:hover { color: var(--accent); border-color: var(--accent); }
  .text-btn.delete:hover   { color: var(--err);    border-color: var(--err); }
  .text-btn.delete.confirm {
    color: var(--err);
    border-color: var(--err);
    background: rgba(255, 128, 128, 0.08);
  }
</style>
