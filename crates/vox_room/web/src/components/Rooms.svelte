<script>
  import { room, enterRoom } from '../lib/room.svelte.js';
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

  /// What the interpreter strip says it is doing. All the way down (or
  /// muted) it is off: nobody is spoken over, and what people say is
  /// still put into the other languages to be read.
  function interpreting() {
    const s = room.strips[room.interpreter?.strip] ?? { gain: 1, mute: false };
    return s.mute || s.gain <= 0 ? 'off' : 'on';
  }

  // Who is in a room you are not in. The count is on the row; this is
  // the whole of what the row can say, for a tooltip.
  function who(r) {
    if (r.private) return `Just you and ${room.agent.name}. Nobody else can come in.`;
    if (!r.here.length) return 'Nobody here.';
    return `${r.here.join(', ')} ${r.here.length === 1 ? 'is' : 'are'} here.`;
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
            <Strip key={room.agent.strip} label={room.agent.name} agent />
            {#if room.interpreter}
              <!-- One fader for every language being interpreted. All the
                   way down it is off: nobody is spoken over, and what
                   people say is still put into the other languages to be
                   read. -->
              <Strip key={room.interpreter.strip} label="Interpreter" agent doing={interpreting()} />
            {/if}
            <p class="hint">
              {#if room.alone}
                This room is yours: nobody else can come into it, and nothing said here
                reaches the rest of the house.
              {:else}
                Levels and mutes here change this room for everyone in it, and a muted
                person isn't transcribed. You never hear yourself.
              {/if}
              {#if room.interpreter}
                The louder the interpreter, the further the voices it is speaking over drop
                under it; all the way down turns it off, and the words are still written
                down for everyone.
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
  .rooms { padding: 0; overflow-y: auto; }
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
  .name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count {
    font-size: 11px;
    font-weight: 500;
    border-radius: 999px;
    padding: 0 6px;
    background: var(--bubble);
    color: var(--muted);
  }
  .room > button.on .count { background: var(--accent-ink); color: var(--accent); }
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
</style>
