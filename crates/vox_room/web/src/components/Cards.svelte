<script>
  import { room, discardCard } from '../lib/room.svelte.js';

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<!-- The private area: the server sends these events to this person's tabs
     and nobody else's, so nothing here is in another browser at all. -->
<section class="private" aria-label="Just for you">
  <h2>
    Just for you
    {#if room.cards.length}<span class="count">{room.cards.length}</span>{/if}
  </h2>
  {#if room.cards.length === 0}
    <p class="hint">
      Nothing yet. What {room.agent.name} means for you alone appears here, and
      on no one else's screen.
    </p>
  {/if}
  {#each room.cards as c (c.id)}
    <article class="card">
      <div class="top">
        <span class="title">{c.title}</span>
        <span class="time">{time(c.at)}</span>
        <button class="x" title="Discard this card" aria-label="Discard {c.title}"
          onclick={() => discardCard(c.id)}>×</button>
      </div>
      <p class="body">{c.body}</p>
    </article>
  {/each}
</section>

<style>
  .private {
    display: flex;
    flex-direction: column;
    gap: 8px;
    border: 1px dashed color-mix(in srgb, var(--agent) 55%, var(--line));
    border-radius: 10px;
    padding: 10px;
    background: color-mix(in srgb, var(--agent) 6%, transparent);
  }
  h2 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--agent);
    margin: 0;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .count {
    font-size: 11px;
    letter-spacing: 0;
    color: var(--accent-ink);
    background: var(--agent);
    border-radius: 999px;
    padding: 0 6px;
  }
  .hint { font-size: 12px; color: var(--muted); margin: 0; }
  .card {
    border: 1px solid color-mix(in srgb, var(--agent) 45%, var(--line));
    border-radius: 8px;
    background: var(--panel);
    padding: 8px 10px;
    /* A long card scrolls inside itself, so it can't push the mixer and
       the games off the sidebar. */
    max-height: 30vh;
    overflow-y: auto;
    animation: dealt 1.2s ease-out;
  }
  .top { display: flex; align-items: baseline; gap: 6px; }
  .title { font-weight: 650; flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .time { font-size: 11px; color: var(--muted); }
  .x { padding: 0 5px; line-height: 1.3; color: var(--muted); }
  .body {
    margin: 6px 0 0;
    font-size: 14px;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  @keyframes dealt {
    0% { box-shadow: 0 0 0 3px color-mix(in srgb, var(--agent) 45%, transparent); }
    100% { box-shadow: 0 0 0 0 transparent; }
  }
  @media (prefers-reduced-motion: reduce) { .card { animation: none; } }
</style>
