<script>
  import { room, enterRoom } from '../lib/room.svelte.js';

  // Which room you are in, top left. One server holds several, and
  // pressing one of these moves you without the audio stopping: the
  // socket stays where it is and the server sends back the room it has
  // put you in.
  //
  // The count is who is in that room right now, you included. Your own
  // room never shows one: it is you and the AI, which is what its name
  // says.
  function who(r) {
    if (r.private) return `Just you and ${room.agent.name}. Nobody else can come in.`;
    if (!r.here.length) return 'Nobody here.';
    return `${r.here.join(', ')} ${r.here.length === 1 ? 'is' : 'are'} here.`;
  }
</script>

{#if room.rooms.length > 1}
  <div class="rooms seg" role="radiogroup" aria-label="Which room you are in">
    {#each room.rooms as r (r.id)}
      <button role="radio" aria-checked={r.id === room.where} class:on={r.id === room.where}
        title={who(r)} onclick={() => enterRoom(r.id)}>
        {r.title}
        {#if !r.private && r.here.length}<span class="count">{r.here.length}</span>{/if}
      </button>
    {/each}
  </div>
{/if}

<style>
  /* The same segmented control the mode buttons use: the two sit in the
     same bar and do the same kind of thing. */
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .seg button {
    border: 0;
    border-radius: 0;
    padding: 5px 10px;
    white-space: nowrap;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
  }
  .seg button.on { background: var(--accent); color: var(--accent-ink); }
  .count {
    font-size: 11px;
    font-weight: 500;
    border-radius: 999px;
    padding: 0 6px;
    background: var(--bubble);
    color: var(--muted);
  }
  .seg button.on .count { background: var(--accent-ink); color: var(--accent); }
  /* Room names run long; on a narrow screen let the bar scroll rather
     than push the AI's state and the mode buttons off the end. */
  @media (max-width: 860px) {
    .rooms { max-width: 100%; overflow-x: auto; }
  }
</style>
