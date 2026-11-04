<script>
  import { room, command, enterRoom, setInterpreter, theAI } from '../lib/room.svelte.js';
  import Strip from './Strip.svelte';

  // Two things, in one component because they are one idea: which room
  // you are in, and who is in it.
  //
  // The room you are in is the heading of the whole page -- there is no
  // title bar above this, and this is what was in it. Press it and
  // every room you may go to drops down over the strips; press one and
  // it closes, you are there, and the heading says so. A server can
  // hold a great many rooms, so the list scrolls rather than growing
  // the column.
  //
  // The strips underneath never move. They are the one place to look
  // for who is here and how loud they are, and a list that pushed them
  // down the screen every time you went looking for another room would
  // make you find them again afterwards -- so the list is over them,
  // not above them.
  //
  // Pressing a room takes you there without the audio stopping: the
  // socket stays where it is and the server sends back the room it has
  // put you in.
  let open = $state(false);
  let picker = $state();

  function choose(r) {
    open = false;
    enterRoom(r.id);
  }

  // Before the first `rooms` event there is no list, but there is a
  // room — stand one in for it rather than flash an empty sidebar.
  const list = $derived(
    room.rooms.length
      ? room.rooms
      : [{ id: room.where, title: room.title, private: room.alone, here: [] }],
  );

  // What a voice talking over the bot does to the reply it is in the
  // middle of. Its own setting as well, and never a game's: a game
  // says when a bot answers, not how it is stopped.
  const stop = (bot) => [
    {
      stops: 'interrupted',
      label: 'when interrupted',
      hint: `Talking over ${bot.name} cuts it off where it had got to, and it is shown where you cut in. 300 ms of voice or a first recognised word, so a cough only dips it.`,
    },
    {
      stops: 'finished',
      label: 'when finished',
      hint: `${bot.name} says the whole of its reply: talking over it dips it but never cuts it off, so a cough can't cost you the answer you asked for. What you said over it isn't lost — it is handed over at the pause after, for ${bot.name} to judge whether it was for it. Stop still stops it.`,
    },
  ];

  const stopHint = (bot) =>
    `What somebody talking over ${bot.name} does to the reply it is in the middle of. Yours to set whatever else the room is doing, and Stop always stops it.`;

  // What the room has this one doing, when it is doing something worth
  // a line of its own. When an AI speaks is nobody's control any more
  // -- whether it may answer *you* is your BOTS button, and these are
  // the states the room itself puts a voice into.
  const doing = (bot) =>
    bot.mode === 'never'
      ? {
          what: room.show ? 'In the show' : 'Saying nothing',
          why: room.show
            ? `${bot.name} answers nothing while the curtain is up. Stop ends the show and gives it back.`
            : `${bot.name} is saying nothing at all just now: not when it is named, and not in a game that wants every pause answered.`,
        }
      : room.locked
      ? {
          what: 'Answering every pause',
          why: `${room.game} is being played, and it requires that of every voice in the room.`,
        }
      : bot.engaged
      ? {
          what: 'In a back-and-forth',
          why: `Somebody said its name, so ${bot.name} has the turn at every pause — judging each one — until the room has been quiet a while.`,
        }
      : bot.mode === 'named'
      ? {
          what: 'Waiting to be named',
          why: `${bot.name} stood itself down: it has nothing to say until somebody says its name.`,
        }
      : null;

  // An interpreter is only here while it has something to do: somebody
  // in this room speaking a language somebody else here doesn't read.
  // A room where everyone shares one has nothing to interpret, and a
  // room of your own can't have anyone to interpret to.
  const mine = $derived(room.people.find((p) => p.name === room.you)?.language);
  const interpreted = $derived(
    !!room.interpreter &&
      !!mine &&
      room.people.some((p) => p.name !== room.you && p.language !== mine),
  );

  /// What the interpreter strip says it is doing. All the way down (or
  /// muted) it is off *for you*: you are not spoken over, and what
  /// people say is still put into your language to be read.
  const interpreting = $derived(
    room.myInterpreter.mute || room.myInterpreter.gain <= 0 ? 'off' : 'on',
  );

  // Who is in a room you are not in. The count is on the row; this is
  // the whole of what the row can say, for a tooltip.
  function who(r) {
    const playing = r.game ? ` Playing ${r.game}.` : '';
    if (r.private) return `Just you and ${theAI('and')}. Nobody else can come in.${playing}`;
    if (!r.here.length) return `Nobody here.${playing}`;
    return `${r.here.join(', ')} ${r.here.length === 1 ? 'is' : 'are'} here.${playing}`;
  }
</script>

<!-- Closing it is a click anywhere else, as it is for the history
     menu above the conversation. -->
<svelte:window onclickcapture={(e) => {
    if (open && !picker?.contains(e.target)) open = false;
  }}
  onkeydown={(e) => { if (e.key === 'Escape') open = false; }} />

<!-- The page's heading, and the way to every other room. There is no
     title bar over this: the room you are in is named here, once. -->
<h1 class="pick" bind:this={picker}>
  <button class="current panel" class:on={open} aria-haspopup="listbox" aria-expanded={open}
    title={open ? 'Close the list of rooms' : 'Which room you are in. Press for the others.'}
    onclick={() => (open = !open)}>
    <span class="name">{room.title}</span>
    {#if room.game}<span class="game">{room.game}</span>{/if}
    <span class="caret" aria-hidden="true">▾</span>
  </button>

  {#if open}
    <!-- Over the strips and not above them, so that what is under this
         is where it was when the list opened and where it will be when
         it shuts. -->
    <div class="list" role="listbox" aria-label="Which room to go to">
      {#each list as r (r.id)}
        {@const here = r.id === room.where}
        <button role="option" aria-selected={here} class:on={here} title={who(r)}
          onclick={() => choose(r)}>
          <span class="name">{r.title}</span>
          <!-- A game is something a room is doing. Say which, so a room
               worth walking into looks like one from out here. -->
          {#if r.game}<span class="game" title="{r.game} is being played there.">{r.game}</span>{/if}
          {#if r.private}
            <span class="count yours">yours</span>
          {:else if r.here.length}
            <span class="count">{r.here.length}</span>
          {/if}
        </button>
      {/each}
    </div>
  {/if}
</h1>

<nav class="rooms panel" aria-label="Who is in {room.title}">
  <!-- A puppet show playing. Said here, over the strips, because the
       faders below are where the puppets are and because this is the
       one time the room has set what every voice answers and what
       stops it, itself:
       the line says why they read the way they do. Stop brings it
       down. -->
  {#if room.show}
    <p class="show" title="The AI and the puppets in it answer nothing and finish what they say until the curtain comes down. Stop ends it.">
      <span class="curtain" aria-hidden="true">●</span>
      <span class="title">{room.show.title}</span>
      <span class="at">line {Math.min(room.show.said + 1, room.show.lines)} of {room.show.lines}</span>
    </p>
  {/if}
  <div class="who">
    <!-- Who is in it, and nobody else: somebody invited but
         elsewhere is on the room they are in, with a count. -->
    {#each room.people as p (p.name)}
      <Strip key={p.name} label={p.name} mic={p.mic} muted={p.muted} speaking={p.speaking}
        somebody />
    {/each}
    <!-- The AIs in this room, each with its own fader and its
         own say in when it speaks -- and the puppets, which have a
         fader and a face like everybody else and nothing to set:
         there is nothing behind one to answer with. -->
    {#each room.agents as bot (bot.strip)}
    <Strip key={bot.strip} label={bot.name} doing={bot.state} agent somebody>
      {#snippet children()}
        {#if bot.puppet}
        <!-- A character the AI speaks through. It is in the room on
             the same terms as anybody -- its own name, face and fader
             -- so the one thing to say about it is which it is. -->
        <p class="wearing" title="A puppet: a character {room.agents[0]?.name ?? 'the AI'} speaks through. It has no say of its own, so there is nothing here to set.">
          A puppet
        </p>
        {#if bot.voice}
          <p class="wearing" title="The voice this puppet speaks in.">
            Speaking as {bot.voice}
          </p>
        {/if}
        {:else}
        <!-- When this bot speaks is no longer a control here. Whether
             an AI may answer *you* is your own BOTS switch in the
             footer, and nobody else's: one control for the whole room
             made being answered a thing the room decided about you.
             What is left is the room's own doing -- a game's rule, a
             show, a voice that has stood itself down -- and it is said
             rather than set, because none of it is a thing to press. -->
        {#if doing(bot)}
          {@const d = doing(bot)}
          <p class="wearing" title={d.why}>{d.what}</p>
        {/if}
        <!-- What talking over it does, which really is the bot's: not
             locked by a game either -- a game says when a bot answers,
             not how it is stopped. -->
        <label class="when" title={stopHint(bot)}>
          <span class="what">Stops</span>
          <select value={bot.stops}
            aria-label="When {bot.name} stops speaking"
            onchange={(e) =>
              command({ type: 'stops', bot: bot.name, stops: e.currentTarget.value })}>
            {#each stop(bot) as s (s.stops)}
              <option value={s.stops} title={s.hint}>{s.label}</option>
            {/each}
          </select>
        </label>
        <!-- A cloned voice it is speaking in: one it was asked for
             out loud, or the one settled on in Settings. Not a
             control here -- the picker is in Settings, with the list
             of voices it would be choosing from. It is said here
             because a bot that has stopped sounding like itself must
             say so where everything else about it is said. -->
        {#if bot.voice}
          <p class="wearing" title="The voice {bot.name} is speaking in. Ask it for its own voice back, or settle which one it uses in Settings.">
            Speaking as {bot.voice}
          </p>
        {/if}
        {/if}
      {/snippet}
    </Strip>
    {/each}
    {#if interpreted}
      <!-- Yours, not the room's: you hear the one speaking your
           language and nobody else hears yours. All the way down
           it is off for you, and the words are still written. -->
      <Strip key={room.interpreter.level} label="Interpreter" agent doing={interpreting}
        fader={room.myInterpreter} onfader={setInterpreter} />
    {/if}
    <p class="hint card-chrome">
      {#if room.alone}
        This room is yours: nobody else can come into it, and nothing said here
        reaches the rest of the house.
      {:else}
        Levels and mutes here change this room for everyone in it, and a muted
        person isn't transcribed. You never hear yourself.
      {/if}
      {#if interpreted}
        The interpreter is yours alone: the louder you have it, the further the
        voices it is speaking over drop under it for you, and all the way down
        turns it off — for you, and with the words still written down.
      {/if}
    </p>
    <!-- The console admitting it has a corner. It says nothing, it is
         not read out, and a theme that does not decorate gives it no
         size, so it is not in the column either. -->
    <span class="hatch mark" aria-hidden="true"></span>
  </div>
</nav>

<style>
  /* Where these sit in the grid, and the rule down the column's edge,
     are App's: a scoped class here would outrank App's :global() and
     strand them in a column of their own on a narrow screen. */

  /* The heading. It is a heading and a control at once -- the room you
     are in, named once, and the way to the others -- so it is sized
     like the title it replaced rather than like a button. */
  /* The heading is the box the list hangs out of, so the frame goes on
     the button inside it and not here: a panel's corners are cut with
     `clip-path`, and a clip takes its descendants with it -- the list
     of rooms below would be clipped out of existence. */
  .pick {
    position: relative;
    margin: 0;
    font-size: inherit;
    font-weight: inherit;
    border-bottom: var(--column-rule);
  }
  /* The room you are in, named once, and the way to the others. It
     carries the frame, for the reason above: in a framed theme it is
     the pill at the top of the column, and in a flat one `.panel`
     resolves to nothing and this is the plain heading it always was.
     The lit line under it is composed with the frame's own bevel here
     rather than left to `.panel`, which knows nothing about rails. */
  .current {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    border: 0;
    padding: 9px 12px;
    font-size: 17px;
    font-weight: 600;
    box-shadow: var(--rail), var(--frame-edge);
  }
  /* The colour only: the frame's cut corners are laid into the same
     property, and a whole `background` here would paint over them. */
  .current:hover { background-color: var(--bubble); }
  .current.on { background-color: var(--bubble); }
  .current .caret { margin-left: auto; font-size: 12pt; opacity: 0.6; font-weight: 400; }
  .current.on .caret { transform: rotate(180deg); }
  .name { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  /* Over the strips, not above them. Absolute so that opening it moves
     nothing underneath -- the strips are the one place to look for who
     is here, and a list that pushed them down the screen would make
     you find them again every time you went looking for a room. */
  .list {
    position: absolute;
    top: 100%;
    left: 0;
    right: 0;
    z-index: 20;
    /* A house can hold a great many rooms; the list scrolls rather
       than running off the bottom of the screen. */
    max-height: min(60vh, 24em);
    overflow-y: auto;
    overscroll-behavior: contain;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 0 0 10px 10px;
    box-shadow: var(--shadow);
    padding: 4px;
  }
  .list button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    border: 0;
    border-radius: 7px;
    background: transparent;
    box-shadow: none;
    padding: 6px 8px;
    font-weight: 600;
    font-size: 14px;
  }
  .list button:hover { background: var(--bubble); }
  .list button.on { background: var(--btn-on-bg); color: var(--btn-on-ink); }
  .list button:not(:has(.game)) .name { flex: 1; }
  /* Whatever is left over goes between the names and the count. */
  .list .game { margin-right: auto; }
  .count {
    font-size: 11px;
    font-weight: 500;
    border-radius: 999px;
    padding: 0 6px;
    background: var(--bubble);
    color: var(--muted);
  }
  .list button.on .count { background: var(--btn-on-ink); color: var(--btn-on-bg); }
  /* Long game names must not push the count off the row, so this is
     what gives way first. */
  .game {
    font-size: 11px;
    font-weight: 500;
    color: var(--agent);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .list button.on .game { color: var(--btn-on-ink); opacity: 0.85; }
  /* A room nobody else can come into, said rather than counted: there
     is never anybody in it but you. */
  .yours {
    font-size: 11px;
    font-weight: 500;
    color: var(--muted);
    background: none;
  }

  /* Who is in the room you are in, and how loud. Always here, always
     in the same place, however the list above it is being used. */
  .rooms {
    padding: 10px 10px 12px;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .who { display: flex; flex-direction: column; gap: 7px; }
  /* The note at the foot of the column: what this room is, said once.
     A card in a framed theme, plain text in a flat one -- which is
     what `--card-*` being nothing comes to. */
  .hint {
    font-size: 12px;
    color: var(--muted);
    margin: 4px 0 0;
    padding: var(--card-pad, 0);
  }
  /* Where the hatching goes: the bottom of the column, out of the way
     of everything that means something. */
  .mark {
    display: block;
    width: var(--hatch-w);
    height: var(--hatch-h);
    margin-top: 10px;
  }
  /* The show. One line, and the only thing in this column with a
     border round it: it is the one state here that nobody in the room
     chose and that changes what every control below it is doing. */
  .show {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin: 0 0 6px;
    padding: 3px 7px;
    border: 1px solid var(--accent);
    border-radius: 7px;
    font-size: 12px;
    line-height: 1.5;
  }
  .show .curtain { color: var(--danger); font-size: 9px; }
  .show .title {
    font-weight: 600;
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .show .at { color: var(--muted); font-variant-numeric: tabular-nums; flex: none; }
  /* What the bot above is doing or wearing: said plainly, not
     shouted, because it is a fact about the bot rather than a
     control. */
  .wearing {
    margin: 2px 0 0;
    font-size: 11px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .when { display: flex; align-items: center; gap: 6px; }
  /* Wide enough for "Stops", so the lines in a strip start in the
     same column. */
  .what { font-size: 11px; color: var(--muted); flex: none; min-width: 52px; }
  .when select {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 12px;
    padding: 1px 4px;
    border: 1px solid var(--field-border);
    border-radius: 6px;
    background: var(--field-bg);
    color: var(--ink);
  }
  .when select:disabled { opacity: 0.6; }
</style>
