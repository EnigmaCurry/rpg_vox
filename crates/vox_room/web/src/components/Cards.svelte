<script>
  import { room, discardCard } from '../lib/room.svelte.js';

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<!-- The private half of the room: the server sends these events to this
     person's tabs and nobody else's, so nothing here is in another
     browser at all. -->
<section class="cards" aria-label="Just for you">
  <header>
    <h2>Just for you</h2>
    {#if room.cards.length}<span class="count">{room.cards.length}</span>{/if}
    <span class="only">on no one else's screen</span>
  </header>

  <div class="scroll">
    {#if room.cards.length === 0}
      <p class="empty">
        Nothing yet. What {room.agent.name} means for you alone appears here —
        a secret role, a hand, what only you noticed.
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
  </div>
</section>

<style>
  .cards {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: color-mix(in srgb, var(--agent) 4%, transparent);
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 16px;
    border-bottom: 1px dashed color-mix(in srgb, var(--agent) 45%, var(--line));
  }
  h2 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--agent);
    margin: 0;
  }
  .count {
    font-size: 11px;
    color: var(--accent-ink);
    background: var(--agent);
    border-radius: 999px;
    padding: 0 6px;
  }
  .only { font-size: 12px; color: var(--muted); margin-left: auto; }
  .scroll {
    overflow-y: auto;
    min-height: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 12px;
    /* The same centred band as the conversation, so a card doesn't run
       the width of half an ultrawide. */
    padding: 16px max(16px, calc((100% - 44em) / 2));
  }
  .empty { color: var(--muted); text-align: center; margin: auto; max-width: 26em; }
  .card {
    border: 1px solid color-mix(in srgb, var(--agent) 45%, var(--line));
    border-radius: 10px;
    background: var(--panel);
    padding: 10px 14px;
    animation: dealt 1.2s ease-out;
  }
  .top { display: flex; align-items: baseline; gap: 8px; }
  .title { font-weight: 650; font-size: 16px; flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .time { font-size: 11px; color: var(--muted); }
  .x { padding: 0 6px; line-height: 1.3; color: var(--muted); }
  .body {
    margin: 6px 0 0;
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
