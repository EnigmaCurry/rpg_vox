<script>
  import { room, command, enterRoom, setInterpreter, theAI } from '../lib/room.svelte.js';
  import Strip from './Strip.svelte';

  // The rooms, down the left, and under the one you are in everybody
  // who is in it -- who is in it, that is, and not everybody invited. A list rather than a row of buttons because a server
  // can hold a great many rooms and they have to stay scannable; a tree
  // because who is in a room belongs to that room, and the room you are
  // in is the only one whose people you have any business mixing.
  //
  // Pressing a room takes you there without the audio stopping: the
  // socket stays where it is and the server sends back the room it has
  // put you in.

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

  // Why a bot's mode is what it is, when the room and not a click
  // decided it.
  const modeHint = (bot) =>
    room.locked
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

<nav class="rooms" aria-label="Rooms">
  <div role="radiogroup" aria-label="Which room you are in">
    {#each list as r (r.id)}
      {@const here = r.id === room.where}
      <div class="room" class:here>
        <button role="radio" aria-checked={here} class:on={here} title={who(r)}
          onclick={() => enterRoom(r.id)}>
          <span class="twist" aria-hidden="true">{here ? '▾' : '▸'}</span>
          <span class="name">{r.title}</span>
          <!-- A game is something a room is doing. Say which, so a room
               worth walking into looks like one from out here. -->
          {#if r.game}<span class="game" title="{r.game} is being played there.">{r.game}</span>{/if}
          {#if !r.private && r.here.length}<span class="count">{r.here.length}</span>{/if}
        </button>

        <!-- Only the room you are in opens: these are faders, and a
             fader belongs to the room it is being heard in. -->
        {#if here}
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
        {/if}
      </div>
    {/each}
  </div>
</nav>

<style>
  /* Where it sits in the grid, and the rule down its edge, are App's:
     a scoped class here would outrank App's :global() and strand the
     sidebar in a column of its own on a narrow screen.
     
     No padding of its own and no heading: the rows run edge to edge
     from the top, the way a list of channels does. The nav's own label
     is what says what it is, for anyone who can't see that it is a
     list of rooms. */
  .rooms { padding: 0; overflow-y: auto; overscroll-behavior: contain; }
  .room { display: flex; flex-direction: column; }
  .room > button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    border: 0;
    border-radius: 0;
    padding: 6px 10px;
    background: transparent;
    font-weight: 600;
  }
  .room > button:hover { background: var(--bubble); }
  .room > button.on { background: var(--accent); color: var(--accent-ink); }
  .twist { font-size: 10px; opacity: 0.7; width: 1em; flex: none; }
  .name { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  /* Whatever is left over goes between the names and the count. */
  .room > button .game { margin-right: auto; }
  .room > button:not(:has(.game)) .name { flex: 1; }
  .count {
    font-size: 11px;
    font-weight: 500;
    border-radius: 999px;
    padding: 0 6px;
    background: var(--bubble);
    color: var(--muted);
  }
  .room > button.on .count { background: var(--accent-ink); color: var(--accent); }
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
  .room > button.on .game { color: var(--accent-ink); opacity: 0.85; }
  /* The branch: indented, and hung off a line so the strips read as
     belonging to the room above them rather than following it. The
     right-hand padding is the only inset in here, and it is there so a
     fader doesn't run into the edge of the column. */
  .who {
    display: flex;
    flex-direction: column;
    gap: 7px;
    margin: 5px 0 7px 14px;
    padding: 0 10px 0 8px;
    border-left: 1px solid var(--line);
  }
  .hint { font-size: 12px; color: var(--muted); margin: 0; }
  /* When the bot above speaks, in this room. */
  .when { display: flex; align-items: center; gap: 6px; }
  .what { font-size: 11px; color: var(--muted); flex: none; }
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
