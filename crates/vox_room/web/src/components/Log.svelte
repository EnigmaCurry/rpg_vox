<script>
  import { room, sendText } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';
  import { TYPED_MAX } from '../lib/names.js';

  let box;
  let stick = true;
  let draft = $state('');

  // The language you are reading in, and whose language each person is
  // speaking: between them they say whether a message needs translating
  // for you, and so whether to wait for one.
  const roomLanguage = $derived(room.languages[0] ?? '');
  const mine = $derived(room.lang || roomLanguage);
  const languageOf = (who) =>
    room.people.find((p) => p.name === who)?.language || roomLanguage;

  /// An entry that has a translation for you: which language it was
  /// written in, and what it says in yours (`null` while that is still
  /// coming). `null` for an entry that needs no translation at all.
  ///
  /// The AI writes in the room's own language whatever it was asked in,
  /// so its replies are translated for anyone reading in another; a
  /// typed message is in whatever its author reads in. What people say
  /// out loud is not translated yet.
  function pair(e) {
    const from = e.kind === 'agent' ? roomLanguage : languageOf(e.who);
    if (!from || from === mine) return null;
    if (e.kind !== 'agent' && !e.typed) return null;
    return { from, text: e.translations?.[mine] ?? null };
  }

  // Entries you have switched to the language they were written in. One
  // at a time and by id, so flipping a message back doesn't flip the
  // whole conversation, and so it survives the entry being replaced as
  // the AI speaks. Yours alone: nobody else's screen changes.
  let asWritten = $state({});

  /// Which language to read an entry in. Your own by default -- reading
  /// the reply back in a language you don't speak is the whole point of
  /// having asked -- and the one it was written in once you say so.
  const showing = (e) => (asWritten[e.id] ? 'from' : 'mine');

  function send(ev) {
    ev.preventDefault();
    if (sendText(draft)) draft = '';
  }

  function onscroll() {
    stick = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  }

  $effect(() => {
    // Follow new text while the view is at the bottom.
    room.log.map((e) => e.text).join();
    room.agent.state;
    if (stick && box) queueMicrotask(() => (box.scrollTop = box.scrollHeight));
  });

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<div class="log" bind:this={box} {onscroll}>
  {#if room.log.length === 0}
    <p class="empty">
      {#if room.game}
        Entering <strong>{room.game}</strong>…
      {:else}
        Nothing said yet. Join the audio and start talking; say <strong>{room.agent.name}</strong>
        {room.mode === 'named' ? 'to bring it in' : 'or just talk, and it answers when you pause'}.
        Ask it to design a game with you, then ask it to start, change or delete games by voice.
      {/if}
    </p>
  {/if}
  {#each room.log as e (e.id)}
    {#if e.kind === 'game'}
      {@const g = room.games.find((g) => g.name.toLowerCase() === e.text.toLowerCase())}
      <div class="entry agent game-entry">
        <div class="who"><span class="name">{e.who}</span><span class="time">{time(e.at)} · new game</span></div>
        <div class="card">
          <div class="title">{e.text}</div>
          {#if g}<p class="summary">{g.summary}</p>{/if}
          {#if !g}
            <span class="hint">This game was deleted.</span>
          {:else if room.game}
            <span class="hint">Go back to the lobby to play it.</span>
          {:else}
            <span class="hint">Ask {room.agent.name} to start it when you're ready.</span>
          {/if}
        </div>
      </div>
    {:else if e.kind === 'card'}
      <!-- A card the AI posted for the whole room; it stays in the
           conversation like anything that was said. -->
      <div class="entry agent card-entry">
        <div class="who"><span class="name">{e.who}</span><span class="time">{time(e.at)} · for the room</span></div>
        <div class="card posted">
          <div class="title">{e.title}</div>
          <p class="body">{e.text}</p>
        </div>
      </div>
    {:else}
    <div class="entry" class:agent={e.kind === 'agent'} class:you={e.who === room.you} style="--h: {hue(e.who)}">
      <div class="who">
        <span class="name">{e.who}</span>
        {#if e.typed}<span class="typed" title="Typed, not spoken">typed</span>{/if}
        <span class="time">{time(e.at)}</span>
      </div>
      {#if pair(e)}
        {@const p = pair(e)}
        {@const from = showing(e) === 'from'}
        <!-- One language at a time, with a link to the other: both are
             here, so a translation that went wrong can be seen to have
             gone wrong, without two copies of every line to read past. -->
        <div
          class="text"
          class:partial={from && e.partial}
          class:live={e.live}
          lang={from ? p.from : mine}
        >
          {#if from}
            {#if e.text}{e.text}{:else}<span class="dots"><i></i><i></i><i></i></span>{/if}
          {:else if p.text}
            {p.text}
          {:else}
            <!-- Not translated yet. Deliberately not the original: being
                 handed a language you didn't ask for is the thing this
                 is here to avoid. The link below reaches it. -->
            <span class="pending">{mine}…</span>
          {/if}
        </div>
        {#if e.text || p.text}
          <button type="button" class="swap" onclick={() => (asWritten[e.id] = !from)}>
            {from ? `Show ${mine}` : `Show ${p.from}`}
          </button>
        {/if}
      {:else}
        <div class="text" class:partial={e.partial} class:live={e.live}>
          {#if e.text}{e.text}{:else if e.kind === 'agent'}<span class="dots"><i></i><i></i><i></i></span>{/if}
        </div>
      {/if}
    </div>
    {/if}
  {/each}
  {#if room.agent.state === 'waiting'}
    <div class="entry agent waiting" aria-live="polite">
      <div class="who"><span class="name">{room.agent.name}</span><span class="time">waiting for a pause</span></div>
      <div class="text"><span class="dots"><i></i><i></i><i></i></span></div>
    </div>
  {/if}

  <!-- Last child of the scroller and stuck to its foot, so the grid
       above doesn't have to know the conversation has a composer. -->
  <form class="composer" onsubmit={send}>
    <input
      type="text"
      bind:value={draft}
      maxlength={TYPED_MAX}
      autocomplete="off"
      placeholder="Type instead of saying it — a URL, a name, anything that has to arrive exactly"
      aria-label="Type a message to the room"
    />
    <button type="submit" class="primary" disabled={!draft.trim()}>Send</button>
  </form>
</div>

<style>
  .log {
    overflow-y: auto;
    /* Keep the conversation in a centered band; on a wide monitor the extra
       space becomes equal margins instead of a line you have to scan across. */
    padding: 16px max(16px, calc((100% - 64em) / 2));
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
  }
  .empty { color: var(--muted); text-align: center; margin: auto; max-width: 28em; }
  .card {
    border: 1px solid color-mix(in srgb, var(--agent) 45%, var(--line));
    background: color-mix(in srgb, var(--agent) 8%, var(--bubble));
    border-radius: 12px;
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
  }
  .card .title { font-weight: 650; font-size: 16px; }
  /* A posted card is wider than a bubble: it is meant to be read, not
     heard, and often has lines of its own. */
  .card-entry { max-width: min(46em, 96%); align-self: stretch; }
  .card.posted { width: 100%; }
  .card.posted .body {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    line-height: 1.5;
  }
  .card .summary { margin: 0; color: var(--muted); }
  .hint { color: var(--muted); font-size: 13px; }
  .entry { max-width: min(42em, 92%); }
  .entry.you { align-self: flex-end; }
  .who { font-size: 12px; color: var(--muted); display: flex; gap: 8px; margin: 0 4px 2px; }
  .entry.you .who { justify-content: flex-end; }
  .name { font-weight: 600; color: hsl(var(--h) 55% 45%); }
  .agent .name { color: var(--agent); }
  .text {
    background: var(--bubble);
    border-left: 3px solid hsl(var(--h) 55% 55%);
    padding: 8px 12px;
    border-radius: 10px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .agent .text { border-left-color: var(--agent); }
  .text.partial { color: var(--muted); }
  .text.live { box-shadow: 0 0 0 1px hsl(var(--h) 55% 55% / 40%); }
  .agent .text.live { box-shadow: 0 0 0 1px color-mix(in srgb, var(--agent) 40%, transparent); }
  /* Thinking: an empty reply bubble with bouncing dots and a breathing edge. */
  .agent .text.live:empty, .agent .text.live:has(> .dots) { animation: breathe 1.6s ease-in-out infinite; }
  .dots { display: inline-flex; gap: 5px; padding: 6px 2px; }
  .dots i { width: 8px; height: 8px; border-radius: 50%; background: var(--agent); animation: bounce 1s ease-in-out infinite; }
  .dots i:nth-child(2) { animation-delay: 0.15s; }
  .dots i:nth-child(3) { animation-delay: 0.3s; }
  /* Waiting for the pause: the same bubble, fainter and slower. */
  .waiting .text { animation: none; opacity: 0.6; }
  .waiting .dots i { animation-duration: 1.8s; background: var(--muted); }
  @keyframes bounce { 0%, 60%, 100% { transform: translateY(0); opacity: 0.4; } 30% { transform: translateY(-5px); opacity: 1; } }
  @keyframes breathe {
    0%, 100% { box-shadow: 0 0 0 1px color-mix(in srgb, var(--agent) 40%, transparent); }
    50% { box-shadow: 0 0 0 4px color-mix(in srgb, var(--agent) 20%, transparent); }
  }
  @media (prefers-reduced-motion: reduce) { .dots i, .text { animation: none !important; } }

  /* Typed, not spoken: said quietly beside the name, because the point
     is that there is no recognition error in it. */
  .typed {
    font-size: 10px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    padding: 1px 5px;
    border: 1px solid var(--line);
    border-radius: 999px;
    color: var(--muted);
  }
  /* The way to the other language: a link under the bubble, quiet
     enough to ignore until it is wanted. */
  .swap {
    margin: 3px 4px 0;
    padding: 0;
    border: 0;
    background: none;
    font: inherit;
    font-size: 12px;
    color: var(--muted);
    text-decoration: underline dotted;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .swap:hover { color: var(--ink); }
  /* Block, so it sits under the bubble rather than beside it, and
     pushed over for your own messages, which hang on the right. */
  .swap { display: block; }
  .entry.you .swap { margin-left: auto; }
  .pending { font-style: italic; opacity: 0.7; }

  .composer {
    position: sticky;
    bottom: 0;
    display: flex;
    gap: 8px;
    /* Stuck to the foot of the scroller, so the messages behind it have
       to be covered rather than showing through. */
    background: var(--bg);
    padding: 10px 0 2px;
    margin-top: auto;
  }
  .composer input {
    flex: 1;
    min-width: 0;
    font: inherit;
    color: var(--ink);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 9px 14px;
  }
  .composer input:focus-visible { border-color: var(--muted); outline: none; }
  .composer button { border-radius: 999px; padding: 9px 16px; }
  .composer button:disabled { opacity: 0.45; cursor: default; }
</style>
