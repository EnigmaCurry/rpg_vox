<script>
  // Per-profile audition field. Type text, click Play, hear the profile
  // speak through the pipewire virtual mic. Server-side widget rows back
  // the audio so we can replay a cached take without re-synthesizing —
  // shift-click on Play skips the render round-trip and just replays the
  // last audio the button rendered.
  //
  // The widget id is in-memory only: it never lands in the /state blob,
  // so a page reload starts fresh (and any orphaned server rows die with
  // the next character/profile delete cascade, or on manual cleanup).
  // onDestroy() fires a best-effort DELETE so the common case (user
  // navigates away or deletes the profile) doesn't leave the row behind.

  import { onDestroy } from 'svelte';
  import {
    createWidget,
    updateWidget,
    playWidget,
    deleteWidget,
    stopPlayback,
  } from '../lib/api.js';
  import { scenesState } from '../lib/scenes.svelte.js';

  let { characterId, profileId } = $props();

  // svelte-ignore state_referenced_locally
  let text = $state('');
  // svelte-ignore state_referenced_locally
  let widgetId = $state(null);
  // 'idle' | 'rendering' | 'playing'
  let mode = $state('idle');
  let error = $state('');
  // Held so we can flag "you have a cached take" in the tooltip even
  // after audio has finished playing — presence of widgetId is enough,
  // but Svelte's $derived on it wants explicit dep tracking below.
  const hasCache = $derived(widgetId !== null);

  let renderAbort = null;
  let playAbort = null;

  onDestroy(() => {
    if (renderAbort) { try { renderAbort.abort(); } catch {} renderAbort = null; }
    if (playAbort)   { try { playAbort.abort(); } catch {} playAbort = null; }
    if (widgetId) {
      const id = widgetId;
      widgetId = null;
      // Fire-and-forget: nothing depends on the delete succeeding, and
      // the server row is otherwise idle disk space.
      deleteWidget(id).catch(() => {});
    }
  });

  async function playCached() {
    if (!widgetId) return;
    stopPlayback().catch(() => {});
    mode = 'playing';
    error = '';
    const ac = new AbortController();
    playAbort = ac;
    try {
      await playWidget(widgetId, { signal: ac.signal });
    } catch (e) {
      if (e?.name !== 'AbortError') error = e.message || 'playback failed';
    } finally {
      if (playAbort === ac) playAbort = null;
      if (mode === 'playing') mode = 'idle';
    }
  }

  async function renderAndPlay() {
    const t = text.trim();
    if (!t) { error = 'text is empty'; return; }
    if (mode !== 'idle') return;
    error = '';
    // Kill any in-flight audio from a previous shift-replay before we
    // start rendering fresh — otherwise the last cached take would keep
    // pushing samples into the mic while we wait for the new synth.
    stopPlayback().catch(() => {});
    mode = 'rendering';
    const ac = new AbortController();
    renderAbort = ac;
    try {
      const opts = {
        signal: ac.signal,
        characterId,
        profileId,
        // Explicit projectId so the server picks the right TTS dictionary
        // even before it has to look up the character's own projectId.
        projectId: scenesState.selectedProjectId ?? null,
      };
      // Configs are ignored server-side when characterId is present, but
      // the schema still expects the field to exist so send an empty list.
      const result = widgetId
        ? await updateWidget(widgetId, t, [], opts)
        : await createWidget(t, [], opts);
      if (result.id) widgetId = result.id;
      if (renderAbort === ac) renderAbort = null;
      // Slide straight into playback so the user hears their edit as
      // soon as it's rendered — cache is now valid for the next shift-click.
      mode = 'playing';
      const playAc = new AbortController();
      playAbort = playAc;
      try {
        await playWidget(widgetId, { signal: playAc.signal });
      } catch (e) {
        if (e?.name !== 'AbortError') error = e.message || 'playback failed';
      } finally {
        if (playAbort === playAc) playAbort = null;
      }
    } catch (e) {
      if (e?.name !== 'AbortError') error = e.message || 'render failed';
    } finally {
      if (renderAbort === ac) renderAbort = null;
      if (mode !== 'idle') mode = 'idle';
    }
  }

  function onPlayClick(event) {
    // Shift-click replays the cached take (no synth) whenever we have one.
    // With no cache, shift silently falls through to a fresh render so the
    // user always hears something instead of getting an inert click.
    if (event.shiftKey && widgetId && mode === 'idle') {
      playCached();
      return;
    }
    renderAndPlay();
  }

  function onStopClick() {
    // Aborts both the fetch and the pipewire drain — mirrors the pattern
    // in SpeakCell.stopExternal so a running clip actually falls silent
    // instead of finishing.
    if (playAbort) { try { playAbort.abort(); } catch {} }
    if (renderAbort) { try { renderAbort.abort(); } catch {} }
    stopPlayback().catch(() => {});
    mode = 'idle';
  }

  function onKeydown(e) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      onPlayClick(e);
    }
  }

  const playTitle = $derived(
    mode !== 'idle'
      ? 'Stop'
      : (hasCache
          ? 'Play (Shift-click to replay cached audio without re-synth)'
          : 'Render and play')
  );
</script>

<div class="test-block">
  <div class="test-head">
    <span>Test</span>
    {#if hasCache}
      <span class="test-hint" title="Shift-click Play to replay this cached take without re-synth">
        cached ✓
      </span>
    {/if}
  </div>
  <div class="test-row">
    <input
      type="text"
      class="test-input"
      placeholder="Type something to hear this profile speak…"
      bind:value={text}
      onkeydown={onKeydown}
      disabled={mode === 'rendering'}
      aria-label="Test text for this voice profile"
    />
    <button
      type="button"
      class="test-btn"
      class:busy={mode === 'rendering'}
      class:playing={mode === 'playing'}
      onclick={mode === 'idle' ? onPlayClick : onStopClick}
      disabled={mode === 'idle' && !text.trim()}
      title={playTitle}
      aria-label={playTitle}
    >
      {#if mode === 'rendering'}
        <span class="spinner" aria-hidden="true"></span>
      {:else if mode === 'playing'}
        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
          <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
        </svg>
      {:else}
        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
          <path fill="currentColor" d="M8 5v14l11-7z"/>
        </svg>
      {/if}
    </button>
  </div>
  {#if error}
    <span class="err small">{error}</span>
  {/if}
</div>

<style>
  .test-block {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 8px;
    border: 1px dashed var(--border);
    border-radius: 6px;
    background: rgba(0,0,0,0.15);
  }
  .test-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .test-hint {
    text-transform: none;
    letter-spacing: normal;
    color: var(--accent);
    font-size: 11px;
    font-weight: 500;
  }
  .test-row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .test-input {
    flex: 1;
    min-width: 0;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
  }
  .test-input:focus { outline: none; border-color: var(--accent); }
  .test-input:disabled { opacity: 0.6; cursor: not-allowed; }

  .test-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    padding: 0;
    background: rgba(122, 162, 255, 0.14);
    color: var(--accent);
    border: 1px solid rgba(122, 162, 255, 0.35);
    border-radius: 6px;
    cursor: pointer;
    flex: 0 0 auto;
  }
  .test-btn:hover:not(:disabled) {
    background: rgba(122, 162, 255, 0.28);
    border-color: var(--accent);
  }
  .test-btn:disabled { opacity: 0.4; cursor: not-allowed; }
  .test-btn.playing,
  .test-btn.busy {
    background: rgba(255, 207, 90, 0.18);
    color: #ffcf5a;
    border-color: rgba(255, 207, 90, 0.5);
  }

  .spinner {
    width: 12px;
    height: 12px;
    border: 2px solid rgba(255, 255, 255, 0.2);
    border-top-color: currentColor;
    border-radius: 50%;
    animation: test-spin 0.7s linear infinite;
  }
  @keyframes test-spin { to { transform: rotate(360deg); } }

  .err.small {
    padding: 2px 6px;
    color: var(--err);
    font-size: 11px;
  }
</style>
