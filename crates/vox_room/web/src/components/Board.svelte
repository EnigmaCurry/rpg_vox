<script>
  // One shared desktop, full size, on this screen alone. Opening or
  // closing it changes nothing for anyone else in the room.
  import { screen, showBoard, unwatch } from '../lib/screen.svelte.js';

  const stream = $derived(screen.streams.find((s) => s.id === screen.watching));

  let canvas = $state();
  $effect(() => (canvas ? showBoard(canvas) : undefined));

  function onkeydown(e) {
    if (e.key === 'Escape') unwatch();
  }
</script>

<svelte:window {onkeydown} />

<section class="board" aria-label="{stream?.person ?? 'A'}'s desktop">
  <header>
    <h2>{stream?.person}'s desktop</h2>
    <span class="hint">Only you are looking at this.</span>
    <div class="spacer"></div>
    <button onclick={unwatch}>Close <kbd>Esc</kbd></button>
  </header>
  <!-- The picture keeps its own shape; the frame around it is the room's. -->
  <div class="picture">
    <canvas bind:this={canvas}></canvas>
  </div>
</section>

<style>
  .board {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    background: #000;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 14px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }
  h2 { font-size: 13px; margin: 0; }
  .hint { font-size: 12px; color: var(--muted); }
  .spacer { flex: 1; }
  .picture {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 8px;
  }
  /* A desktop is wider than it is tall and the area rarely matches it:
     fit the picture inside, rather than cropping or stretching it. */
  canvas {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    image-rendering: crisp-edges;
  }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; margin-left: 4px; }
</style>
