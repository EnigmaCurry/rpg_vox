<script>
  import { room } from '../lib/room.svelte.js';
  import { markdown } from '../lib/markdown.js';

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<!-- The cards one person is holding: the server sends these events to
     their tabs and nobody else's, so nothing here is in another browser
     at all. Holding none of them, you are not shown this at all — App
     leaves it out rather than giving the screen over to an explanation
     of what might one day appear in it. -->
<section class="cards" aria-label="Just for you">
  <header>
    <h2>Just for you</h2>
    <span class="count">{room.cards.length}</span>
    <span class="only">on no one else's screen</span>
  </header>

  <div class="scroll">
    <!-- Keyed by when it was dealt as well as which card it is, so one
         the AI has rewritten comes in with the same glow as a new one. -->
    {#each room.cards as c (`${c.id}-${c.at}`)}
      <article class="card">
        <div class="top">
          <span class="title">{c.title}</span>
          <span class="time">{time(c.at)}</span>
        </div>
        <!-- Written to be read, so it is written in markdown: a hand
             is a list, a standing order has the important part in bold.
             `markdown` escapes everything before it builds a single
             tag, so nothing in here can be markup of its own. -->
        <div class="body md">{@html markdown(c.body)}</div>
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
    overscroll-behavior: contain;
    min-height: 0;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 12px;
    /* The same centred band as the conversation, so a card doesn't run
       the width of half an ultrawide. */
    padding: 16px max(16px, calc((100% - 44em) / 2));
  }
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
  .body {
    margin: 6px 0 0;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  @keyframes dealt {
    0% { box-shadow: 0 0 0 3px color-mix(in srgb, var(--agent) 45%, transparent); }
    100% { box-shadow: 0 0 0 0 transparent; }
  }
  @media (prefers-reduced-motion: reduce) { .card { animation: none; } }
</style>
