<script>
  import { room, enterGame, peekGame, theAI, theAIs, waitingAI, pictureOf, sampleOf } from '../lib/room.svelte.js';
  import History from './History.svelte';
  import Face from './Face.svelte';
  import Stage from './Stage.svelte';
  import Seats from './Seats.svelte';
  import { hue } from '../lib/color.js';
  import { markdown } from '../lib/markdown.js';
  import { scaledDown } from '../lib/pixels.js';

  let box;
  let stick = true;

  // The language you are reading in, and whose language each person is
  // speaking: between them they say whether a message needs translating
  // for you, and so whether to wait for one.
  const roomLanguage = $derived(room.languages[0] ?? '');
  const mine = $derived(room.lang || roomLanguage);
  // Not from the roster: that is who is in the room now, and a line
  // outlives its author's standing in it. `speaks` is what everybody
  // the room knows reads and says, here or not.
  const languageOf = (who) => room.speaks[who] || roomLanguage;

  /// An entry that has a translation for you: which language it was
  /// written in, and what it says in yours (`null` while that is still
  /// coming). `null` for an entry that needs no translation at all.
  ///
  /// The AI writes in the room's own language whatever it was asked in,
  /// so its replies are translated for anyone reading in another; a
  /// typed message is in whatever its author reads in; and what people
  /// say out loud is interpreted, when the room is interpreting -- when
  /// it isn't, there is no translation coming and the line is shown as
  /// it was said.
  function pair(e) {
    const from = e.kind === 'agent' ? roomLanguage : languageOf(e.who);
    if (!from || from === mine) return null;
    if (e.kind !== 'agent' && !e.typed && !room.interprets) return null;
    return { from, text: e.translations?.[mine] ?? null };
  }

  // Entries you have switched to the language they were written in. One
  // at a time and by id, so flipping a message back doesn't flip the
  // whole conversation, and so it survives the entry being replaced as
  // the AI speaks. Yours alone: nobody else's screen changes.
  let asWritten = $state({});

  // How big a picture card's square is, in one place: the CSS draws
  // it and `lib/pixels.js` is told it, and the two must agree.
  const SHOT = 192;
  // Pictures this is scaling *down* rather than up, by id -- which
  // decides how each is drawn, see `lib/pixels.js`. By id and not one
  // flag, because a conversation holds both kinds at once: a sprite,
  // a full-resolution picture beside it, and a photograph somebody
  // dropped in under them.
  let smooth = $state({});
  const sized = (id, img) => {
    if (scaledDown(img, SHOT)) smooth = { ...smooth, [id]: true };
  };

  /// Which language to read an entry in. Your own by default -- reading
  /// the reply back in a language you don't speak is the whole point of
  /// having asked -- and the one it was written in once you say so.
  const showing = (e) => (asWritten[e.id] ? 'from' : 'mine');

  /// How far a line has been heard in each language it is being said
  /// in, while it still is being. The room sends an empty one when
  /// there is nothing left to wait for, which is what takes the row
  /// away.
  const tracks = (e) => room.heard[e.id] ?? [];
  const width = (t) => (t.total ? Math.min(100, Math.round((t.ms / t.total) * 100)) : 0);
  /// What is left of it: seconds while it is being said, and otherwise
  /// either that it is over or that it is still being translated.
  const left = (t) =>
    t.done ? 'heard' : t.total ? `${Math.max(1, Math.ceil((t.total - t.ms) / 1000))}s` : 'translating…';

  function onscroll() {
    stick = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  }

  $effect(() => {
    // Follow new text while the view is at the bottom. An archive
    // doesn't grow, so this is about the live conversation; opening one
    // scrolls to its foot once, which is where it was left.
    shown.map((e) => e.text).join();
    // And when a reply grows or loses its row of languages, which is a
    // line of height appearing under the last thing said.
    shown.map((e) => tracks(e).length).join();
    room.agents.map((a) => a.state).join();
    if (stick && box) queueMicrotask(() => (box.scrollTop = box.scrollHeight));
  });

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

  // What is on screen: the conversation the room is having, or one out
  // of its history that you have opened. An old one is read-only by its
  // nature — the room has moved on from it, so there is nothing to say
  // into — and that is the whole of what read-only means here.
  // A line said on the stage is not the room talking, so it is not in
  // here: it is read on the stage, where it was said, and the
  // conversation keeps one folded note of the whole play at the
  // curtain. See `Kind::Line` in room.rs.
  const shown = $derived((room.reading?.log ?? room.log).filter((e) => e.kind !== 'line'));
  const archived = $derived(!!room.reading);

  // A room can have more than one AI in it, and prose written for one
  // of them reads wrong the moment it does. Puppets are not among them:
  // nothing here is a thing to ask one of those to do.
  const ais = $derived(theAIs());
  const one = $derived(ais.length === 1);
  const it = $derived(one ? 'it' : 'them');
  const answers = $derived(one ? 'it answers' : 'they answer');
</script>

<!-- Nothing but the conversation. The box you type into is along the
     foot of the room, beside the button you talk with: saying a thing
     and typing it are the same act done two ways, and this column is
     for what came of either. -->
<div class="log">
  <!-- A playhouse, while one is open: the stage at the top with the
       characters on it, the director's box under it, and then the
       conversation, which is the audience. Stacked above the
       scrollport, because a scene is watched and the cast has to stay
       in front of you while the transcript scrolls past.

       None of it exists without a show. A room with no characters
       standing in it has no parts to it, and everybody in it can hear
       everybody -- see ROOM.md, Puppets. -->
  {#if room.show}
    <Stage />
    <Seats seat="box" title="Director's box"
      why="Where {room.seats?.director ?? 'the AI'} runs the show. It hears what is said in here and nothing from the audience." />
    <Seats seat="audience" title="Audience" size={28}
      why="Watching. Talk as freely as you like — the box cannot hear a word of it." />
  {/if}

  <div class="said" bind:this={box} {onscroll}>
    <History />

    {#if shown.length === 0}
      <p class="empty">
        {#if archived}
          Nothing was ever said in this one.
        {:else if room.game}
          Entering <strong>{room.game}</strong>…
        {:else if ais.every((a) => a.mode === 'never')}
          Nothing said yet. {theAI()} {one ? 'is' : 'are'} set to say nothing at all,
          so the room is yours: join the audio and talk. Change
          <strong>Answers</strong> beside {it} to bring {it} in.
        {:else}
          Nothing said yet. Join the audio and start talking; say <strong>{theAI()}</strong>
          {ais.every((a) => a.mode === 'named')
            ? `to bring ${it} in`
            : `or just talk, and ${answers} when you pause`}.
          Ask {it} to design a game with you, then ask {it} to start, change or delete games by voice.
        {/if}
      </p>
    {/if}
    {#each shown as e (e.id)}
      {#if e.kind === 'game'}
        {@const g = room.games.find((g) => g.name.toLowerCase() === e.text.toLowerCase())}
        <div class="entry agent game-entry">
          <Face who={e.who} agent />
          <div class="said-by">
          <div class="who"><span class="name">{e.who}</span><span class="time">{time(e.at)} · new game</span></div>
          <div class="card">
            <div class="title">{e.text}</div>
            {#if g}<p class="summary">{g.summary}</p>{/if}
            <!-- A game is announced here, so this is where it is played
                 from: the card knows which game it is and whether the
                 room is already in one. Asking out loud does the same. -->
            {#if !g}
              <span class="hint">This game was deleted.</span>
            {:else}
              <div class="doings">
                <button onclick={() => enterGame(g.name)} disabled={!!room.game}
                  title={room.game
                    ? `You're in ${room.game}. Leave the game to play another.`
                    : `Everyone in this room goes into ${g.name}.`}>Play</button>
                <button onclick={() => peekGame(g.name)}
                  title="Show the game's hidden prompt, to you only">Peek</button>
                <span class="hint">
                  {#if room.game}In {room.game}.{:else}Or ask {theAI()} for it out loud.{/if}
                </span>
              </div>
            {/if}
          </div>
          </div>
        </div>
      {:else if e.kind === 'picture'}
        <!-- A picture one of the AIs made. The card goes up the moment
             it starts drawing, so this is a spinner first and the
             picture after -- the room watches it happen and says what
             to change. Numbered, because that number is what somebody
             says out loud and what set_avatar takes. -->
        {@const made = room.pictures[e.id]}
        <div class="entry agent picture-entry">
          <Face who={e.who} agent />
          <div class="said-by">
            <div class="who">
              <span class="name">{e.who}</span>
              <span class="time">{time(e.at)} · {e.text}</span>
            </div>
            <div class="card shot-card">
              <div class="shot" class:waiting={!made?.ready && !made?.failed}>
                {#if made?.ready}
                  <img src={pictureOf(e.id)} alt={e.title} />
                {:else if made?.failed}
                  <span class="broke" title={made.failed}>didn't come</span>
                {:else}
                  <span class="spinner" aria-label="Drawing"></span>
                {/if}
              </div>
              {#if e.title}<p class="asked">{e.title}</p>{/if}
              {#if made?.failed}<p class="hint">{made.failed}</p>{/if}
            </div>
          </div>
        </div>
      {:else if e.kind === 'upload'}
        <!-- A picture somebody dropped in from their own computer.
             The same card as one the AI made, numbered in the same
             sequence -- that number is what gets said out loud and
             what set_avatar takes -- and their line, not the AI's: it
             carries their face and their colour, because bringing
             something in is a thing a person did. -->
        {@const made = room.pictures[e.id]}
        <div class="entry picture-entry" class:you={e.who === room.you} style="--h: {hue(e.who)}">
          <Face who={e.who} />
          <div class="said-by">
            <div class="who">
              <span class="name">{e.who}</span>
              <span class="time">{time(e.at)} · {e.text}</span>
            </div>
            <div class="card shot-card">
              <!-- No spinner branch: it arrived whole or it is not
                   there at all. -->
              <div class="shot" class:photo={smooth[e.id]}>
                {#if made?.ready}
                  <img src={pictureOf(e.id)} alt={e.title} onload={(ev) => sized(e.id, ev.currentTarget)} />
                {:else}
                  <span class="broke">gone</span>
                {/if}
              </div>
              {#if e.title}<p class="asked">{e.title}</p>{/if}
            </div>
          </div>
        </div>
      {:else if e.kind === 'sample'}
        <!-- A few seconds of somebody reading aloud, recorded so a
             voice can be cloned from it. The card goes up the moment
             the room starts listening and says so while it is: it is
             the light on a microphone being recorded, and it is here
             in the conversation where nobody can miss it rather than
             on one person's screen.

             Theirs, not the AI's: it carries their face and their
             colour, because it is their voice. The clip can be played
             by anyone in the room, which is no more than they could
             already hear. -->
        {@const clip = room.samples[e.id]}
        <div class="entry sample-entry" class:you={e.who === room.you} style="--h: {hue(e.who)}">
          <Face who={e.who} />
          <div class="said-by">
            <div class="who">
              <span class="name">{e.who}</span>
              <span class="time">
                {time(e.at)} · {e.text}{clip?.ready ? ` · ${Math.round(clip.secs)}s` : ''}
              </span>
            </div>
            <div class="card clip-card">
              {#if clip?.ready}
                <audio controls preload="none" src={sampleOf(e.id)}></audio>
                {#if clip.said}<p class="asked">“{clip.said}”</p>{/if}
              {:else if clip?.failed}
                <p class="hint">{clip.failed}</p>
              {:else}
                <p class="listening">
                  <span class="rec" aria-hidden="true"></span>
                  {#if e.who === room.you}
                    Recording — read your card aloud.
                  {:else}
                    Recording {e.who}.
                  {/if}
                </p>
                {#if clip?.passage}<p class="asked">{clip.passage}</p>{/if}
              {/if}
            </div>
          </div>
        </div>
      {:else if e.kind === 'programme'}
        <!-- A show that has been performed, folded up. The lines were
             read on the stage while it was on; this is what the
             conversation keeps of it, and it is shut because a scene
             is a few hundred words of somebody else's dialogue and
             would otherwise push everything the room said off the
             screen. -->
        <div class="entry agent card-entry">
          <Face who={e.who} agent />
          <div class="said-by">
          <div class="who"><span class="name">{e.who}</span><span class="time">{time(e.at)} · the play</span></div>
          <details class="card posted programme">
            <summary><span class="title">{e.title}</span><span class="open">the script</span></summary>
            <div class="body md">{@html markdown(e.text)}</div>
          </details>
          </div>
        </div>
      {:else if e.kind === 'card'}
        <!-- A card the AI posted for the whole room; it stays in the
             conversation like anything that was said. -->
        <div class="entry agent card-entry">
          <Face who={e.who} agent />
          <div class="said-by">
          <div class="who"><span class="name">{e.who}</span><span class="time">{time(e.at)} · for the room</span></div>
          <div class="card posted">
            <div class="title">{e.title}</div>
            <!-- Markdown, because a card is the one thing here written
                 to be looked at rather than heard: a table of times, a
                 list of options, the numbers as numbers. Escaped before
                 anything is built from it -- see lib/markdown.js. -->
            <div class="body md">{@html markdown(e.text)}</div>
          </div>
          </div>
        </div>
      {:else}
      <div class="entry" class:agent={e.kind === 'agent'} class:you={e.who === room.you} style="--h: {hue(e.who)}">
        <Face who={e.who} agent={e.kind === 'agent'} />
        <div class="said-by">
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
        {#if tracks(e).length > 0}
          <!-- The same words take a different time to say in each
               language, so whichever one you are listening to, this is
               where the others have got to. It goes when they are all
               done, which is the thing it is here to show. -->
          <div class="tracks" title={e.kind === 'agent'
            ? `How far ${e.who}'s reply has been heard in each language. The turn is over once every language has heard it out.`
            : `How far ${e.who} has been heard in each language. Anyone listening in one of these is hearing it now, a phrase behind.`}>
            {#each tracks(e) as t (t.language)}
              <span class="track" class:done={t.done} class:waiting={!t.total && !t.done}>
                <span class="lang">{t.language}</span>
                <span class="bar"><i style="width: {width(t)}%"></i></span>
                <span class="left">{left(t)}</span>
              </span>
            {/each}
          </div>
        {/if}
        </div>
      </div>
      {/if}
    {/each}
    {#if !archived && waitingAI()}
      <div class="entry agent waiting" aria-live="polite">
        <Face who={waitingAI().name} agent />
        <div class="said-by">
          <div class="who"><span class="name">{waitingAI().name}</span><span class="time">waiting for a pause</span></div>
          <div class="text"><span class="dots"><i></i><i></i><i></i></span></div>
        </div>
      </div>
    {/if}
  </div>
</div>

<style>
  .log {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    /* Keep the conversation in a centered band; on a wide monitor the
       extra space becomes equal margins instead of a line you have to
       scan across. One definition, because what you type has to start
       where what you said starts -- the caret lines up with the names
       above it, and the bar it sits in runs the full width regardless. */
    --gutter: max(16px, calc((100% - 64em) / 2));
  }
  .said {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    /* No padding at the head of it: the bar at the top of the
       conversation is stuck to the scrollport, and anything above it is
       a sliver the entries scroll through on their way past. The bar
       has padding of its own, so nothing is tight against the top. */
    padding: 0 var(--gutter) 5px;
    display: flex;
    flex-direction: column;
    gap: 12px;
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
  /* A posted card has the whole column: it is a thing to look at, and
     a table of four columns wants the width a bubble doesn't. Written
     with both classes so it beats the `.entry` rule below, which has
     the same weight and the last word otherwise. */
  .entry.card-entry { max-width: 100%; align-self: stretch; }
  .card.posted { width: 100%; }
  /* A posted card has the whole column, so the column it is in has to
     fill what the face leaves of the row. */
  .card-entry .said-by { width: 100%; }
  /* The programme. A card that is shut: the summary is the one line
     saying a play happened, and opening it is asking to read it. */
  .programme > summary {
    display: flex;
    align-items: baseline;
    gap: 8px;
    cursor: pointer;
    list-style: none;
  }
  .programme > summary::-webkit-details-marker { display: none; }
  .programme > summary::before {
    content: '▸';
    color: var(--muted);
    font-size: 11px;
  }
  .programme[open] > summary::before { content: '▾'; }
  .programme > summary .title { font-weight: 650; }
  .programme > summary .open { margin-left: auto; font-size: 11px; color: var(--muted); }
  .programme[open] > .body { margin-top: 8px; }
  .card.posted .body {
    margin: 0;
    /* The card is a column of flex items that line up on the left, so
       this one has to be told to fill it: left to itself it is as wide
       as what is in it, and a table would size the body rather than
       the body holding the table. */
    width: 100%;
    overflow-wrap: anywhere;
    line-height: 1.5;
  }
  .card .summary { margin: 0; color: var(--muted); }
  /* A picture is looked at, so it gets room -- but not the whole
     column: a 128px sprite blown across 42em is a blurry poster.
     `SHOT` above is this number; they are read together. */
  .shot {
    width: 192px;
    height: 192px;
    border-radius: 10px;
    overflow: hidden;
    display: grid;
    place-items: center;
    background: var(--bubble);
    border: 1px solid var(--line);
    align-self: center;
  }
  .shot img {
    width: 100%;
    height: 100%;
    /* A sprite is drawn at the size it means; scaling it up without
       smoothing is the whole point of the form. */
    image-rendering: pixelated;
    object-fit: contain;
    display: block;
  }
  /* A picture the card is making smaller -- one drawn at full
     resolution, or a photograph. Smoothing is what that wants; the
     rule above is about a sprite, which is the other direction. */
  .shot.photo img { image-rendering: auto; }
  .shot.waiting { background: color-mix(in srgb, var(--agent) 7%, var(--bubble)); }
  .shot .broke { color: var(--muted); font-size: 13px; }
  /* Something is happening and it is going to take a while: a quiet
     ring rather than anything that demands to be watched. */
  .spinner {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    border: 3px solid color-mix(in srgb, var(--agent) 25%, transparent);
    border-top-color: var(--agent);
    animation: spin 0.9s linear infinite;
  }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) {
    .spinner { animation-duration: 2.4s; }
  }
  /* What it was asked for, under it: the words to change when the
     next one is asked for. */
  .asked { margin: 0; color: var(--muted); font-size: 13px; }
  /* A picture card is as wide as its picture. It is not a posted
     card -- those hold tables and want the column -- so it must not
     inherit the rule that gives them it. */
  .card.shot-card { width: max-content; max-width: 100%; gap: 6px; }
  .hint { color: var(--muted); font-size: 13px; }
  /* A clip is as wide as the player it holds, like a picture card is
     as wide as its picture. */
  .card.clip-card { width: max-content; max-width: 100%; gap: 6px; }
  .clip-card audio { width: min(20rem, 100%); height: 32px; }
  .clip-card .asked { max-width: 20rem; }
  .listening { margin: 0; font-size: 13px; display: flex; align-items: center; gap: 6px; }
  /* The one red thing in the room, and it means exactly one thing:
     somebody's microphone is being recorded right now. */
  .rec {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #d8463c;
    animation: recording 1.6s ease-in-out infinite;
  }
  @keyframes recording {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.25; }
  }
  @media (prefers-reduced-motion: reduce) {
    .rec { animation: none; }
  }
  .doings { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-top: 8px; }
  .doings button { padding: 2px 10px; font-size: 13px; }
  /* An entry is its face and then everything that was said: the
     square is a column of its own so that the names, the bubbles and
     the rows under them all start at the same place whether or not
     somebody has a picture. */
  .entry { max-width: min(42em, 92%); display: flex; gap: 8px; }
  /* Your own lines sit on the right, so your face does too -- it stays
     on the outside edge, which is where every one of them is. */
  .entry.you { align-self: flex-end; flex-direction: row-reverse; }
  /* The column beside the face. `min-width: 0` so a long word or a
     wide table shrinks it rather than pushing the face off. */
  .said-by { min-width: 0; flex: 1; display: flex; flex-direction: column; align-items: flex-start; }
  /* Beside the name rather than the bubble: the name is a line of
     12px text above the words, and the square lines up with it. */
  .entry > :global(.face) { margin-top: 14px; }
  .entry.you .said-by { align-items: flex-end; }
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

  /* Where each language has got to, under the bubble: one small bar per
     language, and the row goes when every one of them is done. */
  .tracks { display: flex; flex-wrap: wrap; gap: 4px 12px; margin: 4px 4px 0; font-size: 11px; color: var(--muted); }
  .entry.you .tracks { justify-content: flex-end; }
  .track { display: inline-flex; align-items: center; gap: 5px; }
  .lang { font-variant-caps: all-small-caps; letter-spacing: 0.04em; }
  .bar { width: 46px; height: 4px; border-radius: 2px; background: var(--line); overflow: hidden; }
  .bar i { display: block; height: 100%; background: var(--agent); transition: width 200ms linear; }
  .left { font-variant-numeric: tabular-nums; }
  .track.done { color: var(--ok); }
  .track.done .bar i { background: var(--ok); }
  .track.waiting .left { font-style: italic; }
  @media (prefers-reduced-motion: reduce) { .bar i { transition: none; } }

</style>
