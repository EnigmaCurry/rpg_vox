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

  // When a bot speaks. It is that bot's setting in that room, so it
  // lives under that bot in that room, and each of them has its own.
  const modes = (bot) => [
    {
      mode: 'never',
      label: 'never',
      hint: `${bot.name} says nothing at all: not when it is named, and not in a game that wants every pause answered. It stays in the room, on its fader, until this is changed back.`,
    },
    {
      mode: 'named',
      label: 'when named',
      hint: `${bot.name} says nothing until someone says its name.`,
    },
    {
      mode: 'auto',
      label: 'when it fits',
      hint: `${bot.name} is shown every pause and decides for itself whether the moment is its own, so it can sit in a conversation without answering every gap.`,
    },
    {
      mode: 'always',
      label: 'every pause',
      hint: `${bot.name} answers whenever the room goes quiet, with no judgment about whose turn it is.`,
    },
  ];

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

  // Why a bot's mode is what it is, when the room and not a click
  // decided it.
  const modeHint = (bot) =>
    bot.mode === 'never'
      ? `${bot.name} is set to say nothing at all, which is nobody's to override — not a game's, and not its own name's.`
      : room.locked
      ? `${room.game} decides when ${bot.name} answers, so this can't be changed until the room leaves the game.`
      : bot.engaged
        ? `You said its name, so ${bot.name} has the turn at every pause — judging each one — until the room has been quiet a while. Say "thanks" or "goodbye" and it goes back to waiting for its name.`
        : `When ${bot.name} speaks in this room.`;

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
  <button class="current" class:on={open} aria-haspopup="listbox" aria-expanded={open}
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

<nav class="rooms" aria-label="Who is in {room.title}">
  <div class="who">
    <!-- Who is in it, and nobody else: somebody invited but
         elsewhere is on the room they are in, with a count. -->
    {#each room.people as p (p.name)}
      <Strip key={p.name} label={p.name} mic={p.mic} muted={p.muted} speaking={p.speaking} />
    {/each}
    <!-- The AIs in this room, each with its own fader and its
         own say in when it speaks. -->
    {#each room.agents as bot (bot.strip)}
    <Strip key={bot.strip} label={bot.name} doing={bot.state} agent>
      {#snippet children()}
        <!-- When this bot speaks, in this room. -->
        <!-- A row of three buttons wants more width than this
             column has, and the names are the whole point: a
             "Named / Fits / Every" that fits is a control you
             have to already understand. This reads as what it
             is — Vox answers when named. -->
        <label class="when" title={modeHint(bot)}>
          <span class="what">
            Answers{#if bot.engaged}<span class="dot" aria-hidden="true"></span>{/if}
          </span>
          <select value={bot.mode} disabled={room.locked}
            aria-label="When {bot.name} speaks"
            onchange={(e) =>
              command({ type: 'mode', bot: bot.name, mode: e.currentTarget.value })}>
            {#each modes(bot) as m (m.mode)}
              <option value={m.mode} title={m.hint}>{m.label}</option>
            {/each}
          </select>
        </label>
        <!-- And what talking over it does: the other half of the same
             question, so it sits directly under it. Not locked by a
             game -- a game says when a bot answers, not how it is
             stopped. -->
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
    <p class="hint">
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
  </div>
</nav>

<style>
  /* Where these sit in the grid, and the rule down the column's edge,
     are App's: a scoped class here would outrank App's :global() and
     strand them in a column of their own on a narrow screen. */

  /* The heading. It is a heading and a control at once -- the room you
     are in, named once, and the way to the others -- so it is sized
     like the title it replaced rather than like a button. */
  .pick {
    position: relative;
    margin: 0;
    font-size: inherit;
    font-weight: inherit;
    border-bottom: 1px solid var(--line);
  }
  .current {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    border: 0;
    border-radius: 0;
    background: transparent;
    padding: 9px 12px;
    font-size: 17px;
    font-weight: 600;
  }
  .current:hover { background: var(--bubble); }
  .current.on { background: var(--bubble); }
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
    box-shadow: var(--shadow), 0 10px 30px rgb(0 0 0 / 12%);
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
    padding: 6px 8px;
    font-weight: 600;
    font-size: 14px;
  }
  .list button:hover { background: var(--bubble); }
  .list button.on { background: var(--accent); color: var(--accent-ink); }
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
  .list button.on .count { background: var(--accent-ink); color: var(--accent); }
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
  .list button.on .game { color: var(--accent-ink); opacity: 0.85; }
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
  .hint { font-size: 12px; color: var(--muted); margin: 4px 0 0; }
  /* When the bot above speaks, in this room. */
  .when { display: flex; align-items: center; gap: 6px; }
  /* Wide enough for "Answers" with its dot, so the row under it lines
     up with it rather than starting a column of its own. */
  .what { font-size: 11px; color: var(--muted); flex: none; min-width: 52px; }
  .when select {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 12px;
    padding: 1px 4px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--panel);
    color: var(--ink);
  }
  .when select:disabled { opacity: 0.6; }
  /* A back-and-forth the room started, rather than a mode anyone picked. */
  .what .dot {
    display: inline-block;
    width: 5px;
    height: 5px;
    margin-left: 4px;
    border-radius: 50%;
    background: var(--agent);
    animation: pulse 2s ease-in-out infinite;
  }
  @keyframes pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) { .what .dot { animation: none; } }
</style>
