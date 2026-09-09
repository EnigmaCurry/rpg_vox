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

  import { onMount } from 'svelte';
  import { createTake, deleteTake, playWidget, selectTake, stopPlayback } from '../lib/api.js';

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
  } = $props();

  let error = $state('');
  // Ord of a take currently draining through the mic. Null when idle so we
  // can hide the stop indicator without an extra state flag.
  let playingOrd = $state(null);
  // Ord of a take currently rendering (or `-1` while the very first take
  // is being auto-generated). Prevents double-clicks piling on ⟳ before
  // the previous render lands.
  let renderingOrd = $state(null);

  const selectedOrd = $derived(block?.selected_take ?? null);
  const takes = $derived(block?.takes ?? []);
  const hasTakes = $derived(takes.length > 0);

  onMount(() => {
    // Fresh assistant turns land with zero takes per block; the auto-render
    // policy fires a single POST per block on mount so a reply becomes
    // playable as soon as TTS finishes, without user input. Rehydrated
    // turns from a page reload already have takes — no work to do.
    if (autoRender && !hasTakes && renderingOrd === null) {
      renderNewTake();
    }
  });

  async function playTake(t) {
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
    playingOrd = t.ord;
    try {
      await playWidget(t.widget_id);
    } catch (e) {
      if (e?.name !== 'AbortError') error = `play: ${e.message || e}`;
    } finally {
      if (playingOrd === t.ord) playingOrd = null;
    }
  }

  async function renderNewTake() {
    if (renderingOrd !== null) return;
    error = '';
    // Bumped to a placeholder so the button gets a spinner immediately —
    // the actual ord comes back from the server on success and replaces it.
    renderingOrd = -1;
    try {
      const { take } = await createTake(block.id, []);
      const updated = {
        ...block,
        takes: [...takes, take],
        selected_take: take.ord,
      };
      onblockchange?.(updated);
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
    stopPlayback().catch(() => {});
    playingOrd = null;
  }
</script>

<span class="speech" title={block?.text || ''}>
  <span class="text">&ldquo;{block?.text || ''}&rdquo;</span>
  {#if hasTakes}
    {#each takes as t (t.id)}
      <button
        class="take"
        class:selected={t.ord === selectedOrd}
        class:playing={t.ord === playingOrd}
        onclick={() => playTake(t)}
        oncontextmenu={(ev) => onDeleteTake(t, ev)}
        title={`Take ${t.ord + 1} — click to play & select, right-click to delete`}
      >
        <svg viewBox="0 0 24 24" class="ico" aria-hidden="true">
          <path fill="currentColor" d="M8 5v14l11-7z" />
        </svg>
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
  {#if playingOrd !== null}
    <button class="stop" onclick={onStopClick} title="Stop playback" aria-label="Stop playback">
      <svg viewBox="0 0 24 24" class="ico" aria-hidden="true">
        <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
      </svg>
    </button>
  {/if}
  {#if error}<span class="err">{error}</span>{/if}
</span>

<style>
  .speech {
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
  }
  .text {
    font-style: italic;
    color: var(--text);
    max-width: 60ch;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .take, .redo, .stop {
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
  }
  .take:hover, .redo:hover:not(:disabled), .stop:hover {
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
