<script>
  // A single "design a speech clip" cell. Not connected to the mic — the clip
  // is rendered on the server, streamed to the browser as WAV, cached in a
  // Blob URL, and played through browser speakers. Sending the clip to
  // Discord (via the mic) will be a separate action later.
  //
  // States:
  //   empty     — just the + button
  //   editing   — form is unlocked; play synthesizes
  //   rendering — synthesis in flight; textareas + play locked; guessed bar
  //   playing   — cached audio playing through <audio>; deterministic bar
  //   rendered  — locked textareas; play replays cached blob; double-click
  //               a textarea to edit; if edited, X to cancel back to rendered

  import { onDestroy, onMount, tick } from 'svelte';
  import { createWidget, updateWidget, deleteWidget, fetchWidget } from '../lib/api.js';
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
    initialStyleId = null,
    initialWidgetId = null,
    startEditing = false,
    onchange = null,
    ondelete = null,
    // Fired after a successful render so the parent (a Lane) can drive its
    // own <audio> element for sequential playback. Transient — blob URLs
    // don't survive reloads, so they're never persisted.
    onaudio = null,
    // Called once on mount with a handle the parent can use to drive this
    // cell from outside — { playToEnd, stop, isReady } — and once on
    // unmount with `null` so the parent can forget the handle. Enables
    // scene-level playback to run each cell's own <audio> + progress bar
    // in sequence instead of a shared player.
    bindPlayer = null,
    // Fired with `true` whenever the cell enters an editing form (fresh
    // typing, or Edit-clicked on a rendered clip) and `false` when it leaves.
    // Lets the parent know a cell is holding unsaved input so scene-level
    // Play/Render buttons can gate on that.
    onedit = null,
    // Per-clip voice payload forwarded to the server on render:
    // `{ speaker, language, instruct }`. The parent resolves `instruct` from
    // the character's selected style, so this cell no longer edits it.
    voice = {},
    // Available voice styles for this cell's character: `[{id, name}]`.
    // Rendered as a dropdown that picks which style's instruction the
    // server uses. Empty list → dropdown is hidden.
    styles = [],
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
  // user doesn't hit "play" before the blob URL exists.
  // svelte-ignore state_referenced_locally
  let hydrating = $state(!!initialWidgetId);
  // svelte-ignore state_referenced_locally
  let text = $state(initialText);
  // svelte-ignore state_referenced_locally
  let styleId = $state(initialStyleId);
  let error = $state('');

  // Snapshot of what produced `blobUrl` — used to detect "dirty" edits and
  // to revert on cancel. Seeded from initial props so a hydrated cell knows
  // its "clean" state matches the persisted server row.
  // svelte-ignore state_referenced_locally
  let renderedText = $state(initialWidgetId ? initialText : '');
  // svelte-ignore state_referenced_locally
  let renderedStyleId = $state(initialWidgetId ? initialStyleId : null);
  let blobUrl = $state(null);
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

  let audioEl = $state(null);  // <audio> element bound below
  let textEl  = $state(null);  // <textarea> for `text`, focused on entry

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
    if (blobUrl) URL.revokeObjectURL(blobUrl);
    if (deleteTimer) clearTimeout(deleteTimer);
  });

  // Rehydrate from the server's cached WAV if the parent handed us a
  // widgetId. The row already carries text/instruct via props (mirrored
  // through the /state blob the scene store persists), so all we need is
  // the audio + duration/sample-rate metadata. A 404 means the server-side
  // row or WAV was lost — drop the stale id and drop back to editing so the
  // user can re-render.
  onMount(async () => {
    if (!initialWidgetId) return;
    try {
      const result = await fetchWidget(initialWidgetId);
      blobUrl = URL.createObjectURL(result.blob);
      sampleRate = result.sampleRate || 0;
      durationMs = result.durationMs || 0;
      renderedText = text;
      renderedStyleId = styleId;
      hydrating = false;
      onaudio?.({ blobUrl, durationMs });
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
    const snap = { text, styleId, widgetId };
    const key = JSON.stringify(snap);
    if (key === lastEmit) return;
    lastEmit = key;
    onchange(snap);
  });

  const hasClip = $derived(blobUrl !== null);
  const isDirty = $derived(
    hasClip && (text !== renderedText || styleId !== renderedStyleId)
  );
  const progressPct = $derived(`${(progress * 100).toFixed(1)}%`);

  async function beginEditing() {
    state = 'editing';
    error = '';
    // Textarea only mounts once state flips out of `empty`, so wait a tick
    // for Svelte to render it before focusing.
    await tick();
    textEl?.focus();
  }

  function cancelEdit() {
    text = renderedText;
    styleId = renderedStyleId;
    error = '';
    state = 'rendered';
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
    if (blobUrl) { URL.revokeObjectURL(blobUrl); blobUrl = null; }
    text = '';
    styleId = null;
    renderedText = '';
    renderedStyleId = null;
    durationMs = 0;
    sampleRate = 0;
    widgetId = null;
    progress = 0;
    error = '';
    state = 'empty';
  }

  // ---- Render (server round-trip) ------------------------------------------
  function tickRender() {
    const elapsed = performance.now() - startedAt;
    // Hold at 95% until the response actually lands, then snap to 100.
    progress = Math.min((elapsed / estimatedMs) * 0.95, 0.95);
    rafId = requestAnimationFrame(tickRender);
  }

  async function render({ autoplay = true } = {}) {
    const t = text.trim();
    if (!t) { error = 'text is empty'; return; }
    error = '';
    state = 'rendering';
    progress = 0;
    startedAt = performance.now();
    const voiceInstruct = typeof voice?.instruct === 'string' ? voice.instruct : '';
    estimatedMs = estimateMs(t.length + voiceInstruct.length);
    rafId = requestAnimationFrame(tickRender);

    try {
      // First render → create (server assigns id). Subsequent renders →
      // update in place so the store overwrites the same row + WAV file.
      // If we were hydrated with a stale widgetId (server row deleted out
      // from under us — e.g. sqlite wiped), fall back to create so the
      // user's play action still produces a clip.
      // instruct comes from the character's selected style via `voice`.
      let result;
      try {
        result = widgetId
          ? await updateWidget(widgetId, t, '', voice)
          : await createWidget(t, '', voice);
      } catch (e) {
        if (widgetId && /no widget with id/i.test(e.message || '')) {
          widgetId = null;
          result = await createWidget(t, '', voice);
        } else {
          throw e;
        }
      }
      cancelRaf();
      const elapsedMs = performance.now() - startedAt;
      recordSample(t.length + voiceInstruct.length, elapsedMs);

      if (result.id) widgetId = result.id;
      // Rotate blob URL — release the old one so long sessions don't leak.
      if (blobUrl) URL.revokeObjectURL(blobUrl);
      blobUrl = URL.createObjectURL(result.blob);
      sampleRate = result.sampleRate || 0;
      durationMs = result.durationMs || 0;
      renderedText = t;
      renderedStyleId = styleId;
      onaudio?.({ blobUrl, durationMs });

      progress = 1;
      // Brief flash of the filled render bar, then kick playback so the user
      // hears the freshly-rendered clip without an extra click. The `await
      // tick()` gap also ensures the <audio> element has picked up the new
      // blob URL from Svelte's reactive update before we call .play().
      await tick();
      if (autoplay) {
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
      state = hasClip ? 'rendered' : 'editing';
      progress = 0;
      error = e.message || 'render failed';
    }
  }

  // ---- Play (browser <audio>) ----------------------------------------------
  function tickPlay() {
    if (!audioEl || audioEl.paused || audioEl.ended) { cancelRaf(); return; }
    // audio.duration is authoritative once metadata loads; server-reported
    // durationMs is a fallback so the bar starts moving immediately.
    const total = (audioEl.duration && isFinite(audioEl.duration))
      ? audioEl.duration * 1000
      : (durationMs || 1);
    progress = Math.min((audioEl.currentTime * 1000) / total, 1);
    rafId = requestAnimationFrame(tickPlay);
  }

  async function play() {
    if (!blobUrl || !audioEl) return;
    error = '';
    state = 'playing';
    progress = 0;
    try {
      // On a freshly-loaded blob, currentTime is already 0 but the media may
      // not be ready yet — setting it can throw InvalidStateError. Wrap so
      // auto-play after render doesn't fall over. Subsequent replays are on
      // an already-loaded element and always safe.
      try { audioEl.currentTime = 0; } catch {}
      await audioEl.play();
      rafId = requestAnimationFrame(tickPlay);
    } catch (e) {
      cancelRaf();
      state = 'rendered';
      progress = 0;
      error = e.message || 'playback failed';
    }
  }

  function onAudioEnded() {
    cancelRaf();
    progress = 0;
    state = 'rendered';
  }

  // ---- External driver (scene playback) -----------------------------------
  // Kick the cell's own <audio> + progress animation, then resolve when the
  // clip ends (naturally) or is paused (stopped from outside). The parent
  // can call `.stop()` to interrupt without waiting for the promise.
  async function playToEnd() {
    if (!blobUrl || !audioEl) return;
    await play();
    if (!audioEl) return;
    await new Promise((resolve) => {
      const cleanup = () => {
        audioEl?.removeEventListener('ended', cleanup);
        audioEl?.removeEventListener('pause', cleanup);
        resolve();
      };
      audioEl.addEventListener('ended', cleanup);
      audioEl.addEventListener('pause', cleanup);
    });
  }

  function stopExternal() {
    try { audioEl?.pause(); } catch {}
    cancelRaf();
    progress = 0;
    if (state === 'playing') state = 'rendered';
  }

  onMount(() => {
    bindPlayer?.({
      playToEnd,
      stop: stopExternal,
      isReady: () => !!blobUrl,
      render,
      hasText: () => text.trim().length > 0,
    });
  });
  onDestroy(() => {
    bindPlayer?.(null);
  });

  // ---- Play button router --------------------------------------------------
  // Editing (no clip, or dirty edits): synthesize.
  // Rendered (locked, clean): replay cached blob.
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

  // Dropdown value is always a string (even for the "default" empty selection),
  // so we normalize null ↔ '' when reading/writing the styleId state.
  const styleSelectValue = $derived(styleId ?? '');
  function onStyleSelectChange(ev) {
    const val = ev.currentTarget.value;
    styleId = val === '' ? null : val;
  }
  // Show the picker only if the parent handed us any styles.
  const hasStyles = $derived(Array.isArray(styles) && styles.length > 0);
</script>

<div class="cell"
     class:playing={state === 'playing' || state === 'rendering'}
     class:collapsed={state === 'rendered' || state === 'playing' || (state === 'rendering' && hasClip)}
     class:form-mode={state === 'editing' || (state === 'rendering' && !hasClip)}>
  {#if state === 'empty'}
    <button class="plus" onclick={beginEditing} aria-label="Add speak cell">+</button>

  {:else if state === 'rendered' || state === 'playing' || (state === 'rendering' && hasClip)}
    <!-- Collapsed square: shows as much of the spoken text as fits at the
         current size. Font size scales with the container via cqmin so the
         readable-char-count changes with the widget's rendered size. -->
    <div class="clip-text">{text}</div>

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

        {#if hasStyles}
          <label class="style-picker">
            <span>Style</span>
            <select
              value={styleSelectValue}
              onchange={onStyleSelectChange}
              disabled={state === 'rendering'}
              aria-label="Voice style"
            >
              <!-- Empty value = "use the character's default style". Keeps
                   fresh clips silently on the default until the user opts in. -->
              <option value="">(default)</option>
              {#each styles as st (st.id)}
                <option value={st.id}>{st.name}</option>
              {/each}
            </select>
          </label>
        {/if}
      </div>

      <div class="side">
        <button
          class="play"
          onclick={onPlayClick}
          disabled={state === 'rendering'}
          aria-label={hasClip && !isDirty ? 'Play' : 'Render'}
          title={isDirty ? 'Re-render with new text' : hasClip ? 'Play cached clip' : 'Render'}>
          {#if state === 'rendering'}
            <span class="spinner" aria-hidden="true"></span>
          {:else}
            <svg viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
              <path fill="currentColor" d="M8 5v14l11-7z" />
            </svg>
          {/if}
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

  <audio bind:this={audioEl} src={blobUrl ?? undefined} onended={onAudioEnded} preload="auto"></audio>
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

  .style-picker {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .style-picker select {
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
  .style-picker select:focus { outline: none; border-color: var(--accent); }
  .style-picker select:disabled { opacity: 0.6; cursor: not-allowed; }

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
  .text-btn.cancel:hover   { color: var(--text);   border-color: var(--muted); }
  .text-btn.rerender:hover { color: var(--accent); border-color: var(--accent); }
  .text-btn.delete:hover   { color: var(--err);    border-color: var(--err); }
  .text-btn.delete.confirm {
    color: var(--err);
    border-color: var(--err);
    background: rgba(255, 128, 128, 0.08);
  }

  audio { display: none; }
</style>
