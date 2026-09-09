<script>
  // Inline audio widget rendered in place of a `<speak>…</speak>` block in
  // the assistant's markdown reply. Shows the spoken text once, followed by
  // one small button per rendered "take" (▶1, ▶2, …). Selected take is
  // highlighted. `+` triggers a fresh render (new take) via the server.
  //
  // Playback is the same path as SpeakCell: /widgets/:id/say pushes the
  // cached WAV through the pipewire virtual mic so Discord (or any other
  // consumer) hears it. Playback intentionally never touches the browser's
  // audio device — same reasoning as the SpeakCell comment.

  import { onMount, tick } from 'svelte';
  import { createTake, deleteTake, playWidget, selectTake, stopPlayback } from '../lib/api.js';
  import { activeClip } from '../lib/stores.js';
  import { scrollClipIntoView } from '../lib/scroll.js';

  let {
    block,
    /// Called with the full updated block object after any mutation (add
    /// take, select, delete). Parent merges into the store.
    onblockchange = null,
    /// Set to true once by the parent for blocks that came in on the LATEST
    /// assistant turn — those auto-render their first take on mount so the
    /// user doesn't have to click ▶ N times to hear a fresh reply. Older
    /// hydrated blocks don't auto-render (would burn TTS on every page load).
    autoRender = false,
    /// When this equals `block.ord`, and at least one take has landed, the
    /// selected take auto-plays. Parent moves it forward when the current
    /// block finishes so playback runs in reading order. Null disables the
    /// sequence entirely (older turns, autoplay cancelled by manual click).
    autoPlayOrd = null,
    /// Fired after our auto-play finishes so the parent can advance the
    /// sequence to the next block's ord.
    onAutoAdvance = null,
    /// Fired when the user takes ANY manual action on this block (play,
    /// render, stop, delete). The parent cancels the autoplay sequence
    /// so the user's click isn't preempted by the next queued clip.
    onAutoInterrupt = null,
    /// Agent id whose voice slot the server should consult when synthesizing
    /// a new take (empty configs = use agent's `voice_narrator` or
    /// `voice_character` based on this block's role, falling back to the
    /// hardcoded DSP preset). Null = defer entirely to the server fallback.
    agentId = null,
  } = $props();

  let error = $state('');
  // Ord of a take currently rendering (or `-1` while the very first take
  // is being auto-generated). Prevents double-clicks piling on ⟳ before
  // the previous render lands.
  let renderingOrd = $state(null);
  // Root DOM element for scroll-into-view targeting when this pill becomes
  // the active clip. Reactive so bind:this updates the ref when we mount.
  let rootEl = $state(null);
  // Progress overlay element — width is animated 0→100% over the take's
  // durationMs via inline transition so we don't need per-frame js work.
  let progressEl = $state(null);

  const selectedOrd = $derived(block?.selected_take ?? null);
  const takes = $derived(block?.takes ?? []);
  const hasTakes = $derived(takes.length > 0);
  // Legacy blocks stored before the narrator split default to 'character'.
  const role = $derived(block?.role ?? 'character');
  // Which of OUR takes (if any) is the currently-draining clip. Derived
  // from the global activeClip store so ALL play paths — manual take
  // click, autoplay, Play All, whatever comes next — light this block up
  // uniformly. Null when no take of ours is playing.
  const playingOrd = $derived(
    takes.find((t) => t.widget_id === $activeClip?.widgetId)?.ord ?? null
  );

  onMount(() => {
    // Fresh assistant turns land with zero takes per block; fire the
    // render immediately so it starts on the backend runner alongside
    // any playback that's already running (the TTS runner is split into
    // a backend task + a play task so Synthesize can overtake a
    // still-draining PlayPcm — a Play on block K happens on the play
    // task while Synthesize on block K+1 runs on the backend task in
    // parallel). Ords rendered ahead of time queue up on the play task
    // in ord order via the autoplay effect below.
    if (autoRender && !hasTakes && renderingOrd === null) {
      renderNewTake({ fromAutoRender: true });
    }
  });

  // Kick the linear-swipe progress animation + scroll-into-view whenever
  // OUR pill becomes the active clip. We drive width via a CSS transition
  // (duration = the take's durationMs) so the browser paints one smooth
  // fill without JS on every frame. The negative-blend overlay keeps the
  // pill text readable through the swipe.
  //
  // Dedup on `startedAt` (a fresh timestamp on every play iteration) so a
  // Play-All "replay this same widget" skip re-triggers the animation
  // even though the widget id hasn't changed.
  let lastStartedAt = null;
  $effect(() => {
    const cur = $activeClip;
    const stillPlaying = cur && takes.some((t) => t.widget_id === cur.widgetId);
    if (!stillPlaying) {
      // Reset without transition so the next play starts cleanly at 0.
      if (progressEl) {
        progressEl.style.transition = 'none';
        progressEl.style.transform = 'scaleX(0)';
      }
      lastStartedAt = null;
      return;
    }
    if (cur.startedAt === lastStartedAt) return;  // already animating this run
    lastStartedAt = cur.startedAt;
    // Two-step: snap to 0 with no transition, force reflow, then animate
    // to 1 over durationMs. Without the reflow the browser collapses
    // the two style writes and skips the animation.
    if (progressEl) {
      progressEl.style.transition = 'none';
      progressEl.style.transform = 'scaleX(0)';
      void progressEl.offsetWidth;
      progressEl.style.transition = `transform ${cur.durationMs}ms linear`;
      progressEl.style.transform = 'scaleX(1)';
    }
    scrollClipIntoView(rootEl);
  });

  // Auto-play sequence: when this block IS the current autoplay target,
  // walk it through render → play in a single gated flow so the TTS
  // runner never has more than one Synthesize queued ahead of the next
  // PlayPcm. Concretely:
  //
  //   1. If no takes yet and auto-render is on, fire renderNewTake.
  //      When the take lands, `hasTakes` flips and the effect re-runs.
  //   2. When takes exist and the mic is idle, fire the autoplay of the
  //      selected take. On completion `onAutoAdvance` bumps the parent's
  //      cursor to the next block's ord, and the next block's own effect
  //      picks up from step 1.
  //
  // Two guards keep the effect from re-firing on unrelated reactivity:
  //   * `autoPlayedOrd === autoPlayOrd` — already played this cursor.
  //   * `$activeClip !== null` — something else is on the mic (GM proxy,
  //     user memo, previous block). Wait for it to drain.
  //   * `renderingOrd !== null` — we've already kicked a render for this
  //     cursor; wait for it to land.
  let autoPlayedOrd = $state(null);
  $effect(() => {
    if (autoPlayOrd === null || autoPlayOrd === undefined) return;
    if (block?.ord !== autoPlayOrd) return;
    if ($activeClip !== null) return;

    // Render-first path: no takes, so kick a render. When it lands the
    // effect re-runs into the play branch below. Only trigger once per
    // ord (renderingOrd being non-null handles the in-flight case; the
    // ord check handles the "already tried" case).
    if (!hasTakes) {
      if (autoRender && renderingOrd === null) {
        renderNewTake({ fromAutoRender: true });
      }
      return;
    }

    if (autoPlayedOrd === autoPlayOrd) return;
    autoPlayedOrd = autoPlayOrd;
    autoPlaySelected();
  });

  async function autoPlaySelected() {
    const target = takes.find((t) => t.ord === selectedOrd) ?? takes[0];
    if (!target) return;
    // Reuse playTake so the "click a take -> select + play" behavior is
    // shared. onAutoAdvance fires AFTER playback drains so the next
    // block's own effect doesn't try to double-book the pipewire mic.
    await playTake(target, { fromAutoplay: true });
    onAutoAdvance?.(block.id);
  }

  async function playTake(t, { fromAutoplay = false } = {}) {
    // User-driven click on a take should abort the parent's autoplay
    // sequence — otherwise the next queued clip would preempt the one the
    // user just picked.
    if (!fromAutoplay) onAutoInterrupt?.();
    error = '';
    // Marking "selected on audition": clicking a take auto-pins it. Cheap
    // even when it doesn't change anything (server compares to current
    // selected_take under the hood… well, our handler just writes it).
    if (t.ord !== selectedOrd) {
      try {
        await selectTake(block.id, t.ord);
        onblockchange?.({ ...block, selected_take: t.ord });
      } catch (e) {
        error = `select: ${e.message || e}`;
        return;
      }
    }
    // Publish to the global activeClip so THIS component (via its
    // derived playingOrd) and any offscreen sibling get the same
    // "widget X is playing for Y ms" signal.
    activeClip.set({
      widgetId: t.widget_id,
      durationMs: t.duration_ms || 1000,
      startedAt: performance.now(),
    });
    try {
      await playWidget(t.widget_id);
    } catch (e) {
      if (e?.name !== 'AbortError') error = `play: ${e.message || e}`;
    } finally {
      // Only clear if we still own the active slot — Play All / a fresh
      // click may have already advanced to the next clip before our
      // fetch's finally block runs.
      activeClip.update((cur) => (cur?.widgetId === t.widget_id ? null : cur));
    }
  }

  async function renderNewTake({ fromAutoRender = false } = {}) {
    if (renderingOrd !== null) return;
    // ONLY a user-driven ⟳ click cancels autoplay. The auto-render-on-
    // mount path passes `fromAutoRender: true` so the very act of
    // generating the first take doesn't nuke the queue we're waiting on.
    if (!fromAutoRender) onAutoInterrupt?.();
    error = '';
    // Bumped to a placeholder so the button gets a spinner immediately —
    // the actual ord comes back from the server on success and replaces it.
    renderingOrd = -1;
    try {
      const { take } = await createTake(block.id, [], agentId);
      // FIFO cap: mirror the server's `prune_block_takes` (keep 3 most
      // recent). Ords keep monotonically advancing so the button numbers
      // don't renumber — user still sees they're on take #7.
      const MAX_TAKES = 3;
      const kept = [...takes, take]
        .slice()
        .sort((a, b) => b.ord - a.ord)
        .slice(0, MAX_TAKES)
        .sort((a, b) => a.ord - b.ord);
      const updated = {
        ...block,
        takes: kept,
        selected_take: take.ord,
      };
      onblockchange?.(updated);
      // Manual ⟳: user pressed the button explicitly. Only auto-play
      // when auto-render is on — that toggle is the same "hands-free"
      // opt-in as the first-take path, and off means "I want to click
      // ▶ myself". `fromAutoRender: true` is the on-mount path, which
      // is already covered by the autoplay effect wiring above and
      // shouldn't play here.
      if (!fromAutoRender && autoRender) {
        await playTake(take, { fromAutoplay: true });
      }
    } catch (e) {
      error = `render: ${e.message || e}`;
    } finally {
      renderingOrd = null;
    }
  }

  async function onDeleteTake(t, ev) {
    // Right-click / long-press to delete — button label is single-purpose
    // (play) so the destructive action lives on a modifier click. Confirm
    // dialog to guard against accidental removes.
    ev.preventDefault();
    if (!confirm(`Delete take #${t.ord + 1}?`)) return;
    // Manual destructive action: also cancel autoplay so it doesn't try
    // to keep going into a deleted take right after.
    onAutoInterrupt?.();
    error = '';
    try {
      await deleteTake(t.id);
      const remaining = takes.filter((x) => x.id !== t.id);
      // Server has already fixed up selected_take to max(remaining ords) or
      // null; mirror that here so the UI doesn't briefly point at a gone take.
      let nextSelected = block.selected_take;
      if (nextSelected === t.ord) {
        nextSelected = remaining.length
          ? Math.max(...remaining.map((r) => r.ord))
          : null;
      }
      onblockchange?.({ ...block, takes: remaining, selected_take: nextSelected });
    } catch (e) {
      error = `delete: ${e.message || e}`;
    }
  }

  function onStopClick() {
    // Manual stop: cancel autoplay too so the next queued block doesn't
    // slip in a moment later once the pipewire drain finishes.
    onAutoInterrupt?.();
    stopPlayback().catch(() => {});
    activeClip.set(null);
  }
</script>

<span
  class="speech"
  class:narrator={role === 'narrator'}
  class:playing={playingOrd !== null}
  title={block?.text || ''}
  bind:this={rootEl}
>
  <!-- Linear-swipe progress overlay. Sits above the pill background but
       below the take buttons; mix-blend-mode: difference against the
       overlay's white fill inverts the underlying text so it stays
       readable through the swipe (dark theme → light text ↔ dark). -->
  <span class="progress" bind:this={progressEl} aria-hidden="true"></span>
  {#if role === 'narrator'}
    <span class="text prose">{block?.text || ''}</span>
  {:else}
    <span class="text">&ldquo;{block?.text || ''}&rdquo;</span>
  {/if}
  {#if hasTakes}
    {#each takes as t (t.id)}
      <button
        class="take"
        class:selected={t.ord === selectedOrd}
        class:playing={t.ord === playingOrd}
        onclick={() => (t.ord === playingOrd ? onStopClick() : playTake(t))}
        oncontextmenu={(ev) => onDeleteTake(t, ev)}
        title={t.ord === playingOrd
          ? `Take ${t.ord + 1} — playing (click to stop)`
          : `Take ${t.ord + 1} — click to play & select, right-click to delete`}
      >
        <!-- Same-sized SVG whether idle (▶) or playing (■) so swapping the
             icon doesn't reflow the pill. -->
        {#if t.ord === playingOrd}
          <svg viewBox="0 0 24 24" class="ico" aria-hidden="true">
            <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
          </svg>
        {:else}
          <svg viewBox="0 0 24 24" class="ico" aria-hidden="true">
            <path fill="currentColor" d="M8 5v14l11-7z" />
          </svg>
        {/if}
        <span class="ord">{t.ord + 1}</span>
      </button>
    {/each}
  {/if}
  {#if renderingOrd !== null}
    <span class="spinner" aria-label="rendering" title="Rendering next take…"></span>
  {/if}
  <button
    class="redo"
    onclick={renderNewTake}
    disabled={renderingOrd !== null}
    title={hasTakes ? 'Render another take' : 'Render'}
    aria-label={hasTakes ? 'Render another take' : 'Render'}
  >
    {hasTakes ? '⟳' : '▶'}
  </button>
  <!-- The playing take's button doubles as its own Stop: while it's the
       active clip, clicking it invokes onStopClick instead of a re-play.
       Icon swap happens inline in each take button above so nothing shows
       up here and the pill's width stays constant across play/idle. -->
  {#if error}<span class="err">{error}</span>{/if}
</span>

<style>
  .speech {
    /* Character pill — blue accent, italic quoted text. Reads as a spoken
       line even at a glance. */
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 6px 3px 8px;
    background: rgba(122, 162, 255, 0.10);
    border: 1px solid rgba(122, 162, 255, 0.28);
    border-radius: 999px;
    line-height: 1.35;
    /* Wrap the whole pill together — never split "text" from its take
       buttons across lines. */
    white-space: normal;
    max-width: 100%;
    /* Anchor the progress overlay + clip its swipe to the pill's rounded
       shape. Isolation creates a stacking context so `mix-blend-mode` on
       the overlay only inverts the pill contents, not the whole page. */
    position: relative;
    overflow: hidden;
    isolation: isolate;
  }
  .speech.playing {
    border-color: var(--accent);
  }

  /* Progress overlay: absolutely positioned to cover the pill's full
     padding box, animated via transform: scaleX from 0 → 1.
     Chose transform over width for two reasons:
       (a) transforms are GPU-accelerated and never flake mid-transition,
       (b) `inset: 0` + `transform-origin: left` reliably covers ALL of
           the pill in inline-flex layouts, whereas `width: 100%` on an
           absolutely-positioned child in an inline-flex container can
           land short of the right edge on wrapped multi-line content.
     Translucent accent tint instead of a mix-blend-mode invert — the
     difference-blend flaked on wrapped text in inline-flex, painting
     the second line pure white. This layered approach draws the text
     ABOVE the sweep so readability doesn't depend on any blend mode. */
  .progress {
    position: absolute;
    inset: 0;
    background: rgba(122, 162, 255, 0.35);
    transform-origin: left center;
    transform: scaleX(0);
    pointer-events: none;
    z-index: 0;
  }
  /* Text sits above the sweep so it stays fully legible during playback
     regardless of how many lines it wraps to. */
  .text { position: relative; z-index: 1; }
  /* Narrator pill — warmer amber accent, no italic/quotes, prose-flowing
     text. Visual distinct from character so at a glance you can tell
     which voice will read each region. */
  .speech.narrator {
    background: rgba(255, 190, 120, 0.06);
    border: 1px dashed rgba(255, 190, 120, 0.35);
  }
  .text {
    font-style: italic;
    color: var(--text);
    max-width: 60ch;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .text.prose {
    font-style: normal;
    max-width: 80ch;
    /* Prose reads left-to-right without truncation; longer paragraphs
       should wrap inside the pill rather than get cut off. */
    white-space: normal;
    text-overflow: clip;
    overflow: visible;
  }
  .take, .redo {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    padding: 2px 6px;
    height: 22px;
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 999px;
    cursor: pointer;
    font-size: 11px;
    font-family: inherit;
    font-variant-numeric: tabular-nums;
    /* Keep buttons above the progress overlay so the swipe doesn't invert
       their icon glyphs (would look glitchy) and they stay clickable. */
    position: relative;
    z-index: 2;
  }
  /* Same for the leading text — inversion is nice on prose, but we don't
     want the surrounding action buttons to flicker. Text stays under the
     progress on purpose (that's the whole point of the difference blend). */
  .take:hover, .redo:hover:not(:disabled) {
    color: var(--accent);
    border-color: var(--accent);
    background: rgba(122, 162, 255, 0.08);
  }
  .take.selected {
    color: var(--accent);
    border-color: var(--accent);
    background: rgba(122, 162, 255, 0.18);
  }
  .take.playing {
    color: var(--accent);
    background: rgba(122, 162, 255, 0.30);
    border-color: var(--accent);
    animation: pulse 1s ease-in-out infinite;
  }
  @keyframes pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(122, 162, 255, 0.35); }
    50%      { box-shadow: 0 0 0 3px rgba(122, 162, 255, 0.0); }
  }
  .redo:disabled { opacity: 0.5; cursor: default; }
  .ico { width: 10px; height: 10px; }
  .ord { line-height: 1; }
  .spinner {
    display: inline-block;
    width: 12px;
    height: 12px;
    border: 2px solid rgba(122, 162, 255, 0.25);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
  .err {
    color: var(--err);
    font-size: 11px;
    padding-left: 4px;
  }
</style>
