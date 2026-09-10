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

  let { characterId, profileId, referenceText = '' } = $props();

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
    // Empty input falls back to the reference-transcription placeholder so
    // "hit Play with nothing typed" auditions the same phrase the voice
    // clone was calibrated against — the fastest apples-to-apples listen.
    const t = text.trim() || String(referenceText || '').trim();
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
    // Wall-clock stamp so the perf log below can reject stale records
    // from an earlier render that snuck in (the ring is process-wide).
    const renderStartedAt = Date.now();
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
      // Fire-and-forget: pull the just-completed render's timing tree and
      // dump a breakdown into the JS console. Runs in parallel with the
      // playback below so the perf log lands almost immediately.
      logLatestRenderPerf(renderStartedAt).catch(() => {});
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

  // ---- perf logging ------------------------------------------------------
  //
  // After each fresh render, fetch the most recent RenderRecord from the
  // server's ring buffer (crates/rpg_vox/src/tts/perf.rs) and pretty-print
  // a breakdown to the JS console: full span tree + a summary of where the
  // time went (backend vs FX chain, dominant FX stage). Fire-and-forget —
  // any network or parse error is swallowed so the audition itself never
  // reports "perf failed" back to the user.

  async function logLatestRenderPerf(sinceUnixMs) {
    let record;
    try {
      const resp = await fetch('/perf/renders/latest');
      if (!resp.ok) return;
      record = await resp.json();
    } catch {
      return;
    }
    if (!record || typeof record.total_us !== 'number') return;
    // Ring is process-wide — some other request could have landed a render
    // between our call to createWidget and our fetch. A short grace window
    // (250ms of clock drift + network jitter) rejects those.
    if (record.started_at_unix_ms < sinceUnixMs - 250) {
      console.warn('[perf] latest render appears stale; skipping analysis', {
        recordStartedAt: record.started_at_unix_ms,
        ourStartedAt: sinceUnixMs,
      });
      return;
    }

    const totalUs = record.total_us;
    const totalMs = totalUs / 1000;
    // Realtime multiplier: audio-length / wall-clock. > 1.0 means synth
    // outpaced realtime (10s of audio in 2s = 5.0×). Server may return
    // camelCase or snake_case depending on serde config — accept both.
    const audioFrames = record.audio_frames ?? record.audioFrames ?? null;
    const audioRate = record.audio_sample_rate ?? record.audioSampleRate ?? null;
    const audioMs = audioFrames && audioRate ? (audioFrames * 1000) / audioRate : null;
    const rtMult = audioMs != null && totalMs > 0 ? audioMs / totalMs : null;
    const audioTag = audioMs != null && rtMult != null
      ? ` — ${(audioMs / 1000).toFixed(2)}s audio, ${rtMult.toFixed(2)}× realtime`
      : '';
    const header = `🎙 Test render #${record.id} — ${record.label || '(unlabeled)'} — ${totalMs.toFixed(2)}ms wall${audioTag}`;
    /* eslint-disable no-console */
    console.groupCollapsed(header);

    // Full tree
    printNode(record.root, totalUs, 0);

    // Roll-up: sum every occurrence of a given span name so a multi-voice
    // profile's per-voice costs collapse into a single line each.
    const flat = flattenNodes(record.root);
    const sumByName = new Map();
    for (const n of flat) {
      sumByName.set(n.name, (sumByName.get(n.name) ?? 0) + n.dur_us);
    }
    const backend = sumByName.get('backend.synthesize') ?? 0;
    const dsp = sumByName.get('voice.dsp') ?? 0;
    const effects = sumByName.get('apply_effects') ?? 0;
    const fxChain = sumByName.get('apply_fx_chain') ?? 0;
    const mix = sumByName.get('stereo_mix') ?? 0;

    console.log(
      `Summary — backend ${pct(backend, totalUs)}, dsp ${pct(dsp, totalUs)} ` +
      `(effects ${pct(effects, totalUs)}, fx ${pct(fxChain, totalUs)}), ` +
      `mix ${pct(mix, totalUs)}`
    );

    // Dominant FX stage across all voices — often the reverb, but the
    // point is to catch surprises (e.g. saturation being oddly expensive).
    const fxStages = flat.filter((n) => n.name.startsWith('fx.'));
    const stageTotals = new Map();
    for (const n of fxStages) {
      stageTotals.set(n.name, (stageTotals.get(n.name) ?? 0) + n.dur_us);
    }
    if (stageTotals.size > 0) {
      const stageRows = [...stageTotals.entries()]
        .map(([name, us]) => ({ stage: name, ms: +(us / 1000).toFixed(2), pct: +((100 * us) / totalUs).toFixed(1) }))
        .sort((a, b) => b.ms - a.ms);
      console.log('FX stages (summed across voices):');
      console.table(stageRows);
      const dominant = stageRows[0];
      if (dominant && dominant.pct >= 5) {
        console.log(`⚠ Dominant FX: ${dominant.stage} at ${dominant.ms}ms (${dominant.pct}%)`);
      }
    }

    // The raw record — click to expand and inspect.
    console.log('Raw record:', record);
    console.groupEnd();
    /* eslint-enable no-console */
  }

  function printNode(node, rootUs, depth) {
    const ms = (node.dur_us / 1000).toFixed(2);
    const pctStr = rootUs > 0 ? ((100 * node.dur_us) / rootUs).toFixed(1) : '0.0';
    const indent = '  '.repeat(depth);
    // eslint-disable-next-line no-console
    console.log(`${indent}├ ${node.name.padEnd(26)} ${ms.padStart(9)}ms  (${pctStr.padStart(5)}%)`);
    for (const child of node.children || []) {
      printNode(child, rootUs, depth + 1);
    }
  }

  function flattenNodes(node, out = []) {
    out.push(node);
    for (const child of node.children || []) flattenNodes(child, out);
    return out;
  }

  function pct(us, totalUs) {
    if (!totalUs) return '0.0%';
    return `${((100 * us) / totalUs).toFixed(1)}%`;
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
      placeholder={referenceText ? referenceText : 'Type something to hear this profile speak…'}
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
      disabled={mode === 'idle' && !text.trim() && !String(referenceText || '').trim()}
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
