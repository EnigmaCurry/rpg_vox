<script>
  import { onMount } from 'svelte';
  import { room, command, ask, answer, whoami, leaveAudio, setMuted, talk, busyAI } from './lib/room.svelte.js';
  import { screen, share, stopSharing, supported as canShare } from './lib/screen.svelte.js';
  import Log from './components/Log.svelte';
  import Cards from './components/Cards.svelte';
  import Enter from './components/Enter.svelte';
  import Board from './components/Board.svelte';
  import Settings from './components/Settings.svelte';
  import People from './components/People.svelte';
  import Rooms from './components/Rooms.svelte';

  // Narrow screens show one area at a time; wide ones show all three.
  let view = $state('chat');

  // Nothing dealt to you, no area for it: the column (and its tab) are
  // gone rather than sitting there empty, and the conversation takes the
  // room back. It comes and goes with the cards, since the AI can take
  // the last one back at any moment — in which case whoever was looking
  // at that tab is put back on the conversation.
  const hasCards = $derived(room.cards.length > 0);
  const area = $derived(view === 'cards' && !hasCards ? 'chat' : view);

  const views = $derived([
    { id: 'chat', label: 'Conversation' },
    ...(hasCards ? [{ id: 'cards', label: 'Just for you' }] : []),
    { id: 'room', label: 'Rooms' },
  ]);

  // Only who this browser is. The sockets wait for Join Audio: a tab at
  // the door is not in the room, and nothing of the room's is its
  // business until it is.
  onMount(whoami);

  async function newConversation() {
    const question = room.game
      ? `Restart ${room.game} from the beginning, for everyone?`
      : 'Start a new conversation? The current one is kept in the database but cleared from the screen.';
    if (await ask(question, room.game ? 'Restart' : 'New conversation')) command({ type: 'new_conversation' });
  }

  // "Leave the game", not "back to the lobby": a room may well be
  // called Lobby, and a button that reads like it goes there when it
  // does nothing of the kind is a worse button than a plain one.
  async function lobby() {
    if (await ask(`Leave ${room.game} and take everyone in this room back to the conversation?`, 'Leave the game')) {
      command({ type: 'lobby' });
    }
  }

  let peekDialog = $state();
  $effect(() => {
    if (!peekDialog) return;
    if (room.peek && !peekDialog.open) peekDialog.showModal();
    if (!room.peek && peekDialog.open) peekDialog.close();
  });

  let dialog = $state();
  $effect(() => {
    if (!dialog) return;
    if (room.asking && !dialog.open) dialog.showModal();
    if (!room.asking && dialog.open) dialog.close();
  });

  // Space toggles your mute (or, with push to talk, talks while held),
  // unless you are typing or choosing something.
  //
  // Buttons are deliberately *not* an exception. A button keeps the
  // focus after you click it, so excluding them meant the space bar
  // stopped talking the moment you touched one -- a room in the list, a
  // fader's mute, somebody's screen share -- and the way to get it back
  // was to know to click the background. Space belongs to your
  // microphone everywhere in the room; a focused button is still
  // worked by Enter. `preventDefault` below is what keeps the space bar
  // from pressing that button again while you talk.
  //
  // A dialog is an exception, because there Space is how you answer it.
  const TYPING = 'input, textarea, select, [contenteditable=""], [contenteditable="true"], dialog';
  function spaceKey(e) {
    return e.code === 'Space' && room.audio === 'on' && !e.target.closest(TYPING);
  }
  function onkeydown(e) {
    if (!spaceKey(e)) return;
    e.preventDefault();
    if (room.pushToTalk) talk(true);
    else if (!e.repeat) setMuted(!room.muted);
  }
  function onkeyup(e) {
    if (e.code !== 'Space' || !room.pushToTalk) return;
    e.preventDefault();
    talk(false);
  }
</script>

<svelte:window {onkeydown} {onkeyup} onblur={() => talk(false)} />

<dialog class="ask peek" bind:this={peekDialog} oncancel={(e) => { e.preventDefault(); room.peek = null; }}
  onclick={(e) => { if (e.target === peekDialog) room.peek = null; }}>
  {#if room.peek}
    <h2 class="peek-title">{room.peek.name}</h2>
    <p class="peek-summary">{room.peek.summary}</p>
    <div class="peek-prompt">{room.peek.prompt}</div>
    <div class="ask-actions">
      <span class="hint">Only you can see this. It may spoil the game.</span>
      <button class="primary" onclick={() => (room.peek = null)}>Close</button>
    </div>
  {/if}
</dialog>

<dialog class="ask" bind:this={dialog} oncancel={(e) => { e.preventDefault(); answer(false); }}>
  {#if room.asking}
    <p>{room.asking.text}</p>
    <div class="ask-actions">
      <button onclick={() => answer(false)}>Cancel</button>
      <button class="primary" onclick={() => answer(true)}>{room.asking.yes}</button>
    </div>
  {/if}
</dialog>

{#if room.expired}
  <main class="gate">
    <h1>This link is expired</h1>
    <p>
      An invite link works once, on the browser that uses it — and this browser
      hasn't been in before. Ask whoever runs the room for a new link.
    </p>
  </main>
{:else if room.denied}
  <main class="gate">
    <h1>Voice room</h1>
    <p>You need an invite link to come in. Ask whoever runs the room for one.</p>
  </main>
{:else if room.audio !== 'on'}
  <!-- The door is the page, not a sheet over the room. Until the audio
       is joined there is nothing behind it to see: the room is voice,
       and a conversation you can read but not hear is not being in it. -->
  <Enter />
{:else if room.managing && room.owner}
  <!-- The owner's page, and nobody else's: every endpoint behind it
       asks who is asking, so this only decides what is shown. -->
  <People />
{:else if room.settings}
  <!-- A page, not a sheet: what is on it is set once and not watched,
       and the room goes on running behind it -- you are still in the
       audio and the space bar still works. -->
  <Settings />
{:else}
  <div class="app show-{area}" class:watching={screen.watching != null} class:nocards={!hasCards}>
    <header>
      <!-- Which room you are in. The list on the left says it too, on a
           screen wide enough to show the list -- on a narrow one it is
           behind a tab, and then this is the only thing that does. -->
      <h1>{room.title}</h1>
      {#if room.game}
        <span class="playing" title="{room.game} is being played in {room.title}.">{room.game}</span>
        <button onclick={lobby}>Leave the game</button>
      {/if}
      {#if busyAI()}
        <!-- Whichever of them is talking: Stop cuts the reply that is
             in flight, and in a room with two AIs only one is. -->
        <button class="danger" onclick={() => command({ type: 'stop' })}>Stop {busyAI().name}</button>
      {/if}
      <div class="spacer"></div>
      <button onclick={newConversation}>{room.game ? 'Restart game' : 'New conversation'}</button>
      {#if !room.connected}<span class="offline">reconnecting…</span>{/if}
    </header>

    <nav class="views" aria-label="Which area to show">
      {#each views as v (v.id)}
        <button aria-pressed={area === v.id} class:on={area === v.id} onclick={() => (view = v.id)}>
          {v.label}
          {#if v.id === 'cards'}<span class="badge">{room.cards.length}</span>{/if}
        </button>
      {/each}
    </nav>

    <Rooms />

    {#if hasCards}<Cards />{/if}

    <Log />

    {#if screen.watching != null}<Board />{/if}

    <footer>
      {#if room.audio === 'on'}
        {#if room.pushToTalk}
          <button class="big ptt" class:talking={room.talking} disabled={!room.mic}
            onpointerdown={(e) => { e.currentTarget.setPointerCapture(e.pointerId); talk(true); }}
            onpointerup={() => talk(false)} onpointercancel={() => talk(false)}
            onkeydown={(e) => { if (e.code === 'Enter') { e.preventDefault(); talk(true); } }}
            onkeyup={(e) => { if (e.code === 'Enter') { e.preventDefault(); talk(false); } }}>
            {room.talking ? 'Talking…' : 'Hold to talk'} <kbd>Space</kbd>
          </button>
        {:else}
          <button class="big" class:muted={room.muted} onclick={() => setMuted(!room.muted)} disabled={!room.mic}>
            {room.muted ? 'Unmute' : 'Mute'} <kbd>Space</kbd>
          </button>
        {/if}
        <!-- Your mic on the left, and what you do with the room on the
             right: everything you set once is behind the cog. A footer
             strung with sliders and readouts put the way out next to a
             volume control, and none of it was anything you touched
             twice. -->
        <div class="spacer"></div>
        {#if canShare()}
          <button class="share" class:on={screen.sharing} onclick={() => (screen.sharing ? stopSharing() : share())}
            title={screen.sharing
              ? 'Stop sharing your desktop with the room.'
              : 'Share a window or screen. Everyone sees a thumbnail of it in the mixer and can open it for themselves.'}>
            {screen.sharing ? 'Stop sharing' : 'Share screen'}
          </button>
        {/if}
        <button class="leave" onclick={leaveAudio} title="Leave the audio and go back to the door.">Leave audio</button>
        <button class="cog" aria-label="Settings" aria-pressed={room.settings}
          onclick={() => (room.settings = !room.settings)}
          title="Push to talk, your volume, and what the room gave you to speak into.">
          <svg viewBox="0 0 24 24" width="17" height="17" aria-hidden="true" focusable="false">
            <path fill="currentColor" d="M19.14 12.94a7.07 7.07 0 0 0 0-1.88l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96a7.03 7.03 0 0 0-1.63-.94l-.36-2.54a.5.5 0 0 0-.5-.42h-3.84a.5.5 0 0 0-.49.42l-.36 2.54c-.59.24-1.13.56-1.63.94l-2.39-.96a.5.5 0 0 0-.6.22L2.66 8.84a.5.5 0 0 0 .12.64l2.03 1.58a7.07 7.07 0 0 0 0 1.88L2.78 14.52a.5.5 0 0 0-.12.64l1.92 3.32c.13.22.39.31.6.22l2.39-.96c.5.38 1.04.7 1.63.94l.36 2.54c.04.24.25.42.49.42h3.84c.25 0 .46-.18.5-.42l.36-2.54c.59-.24 1.13-.56 1.63-.94l2.39.96c.22.09.47 0 .6-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58ZM12 15.5A3.5 3.5 0 1 1 15.5 12 3.5 3.5 0 0 1 12 15.5Z" />
          </svg>
        </button>
      {/if}
      {#if room.audioError}<span class="warn">Mic: {room.audioError}</span>{/if}
      {#if screen.error}<span class="warn">Screen: {screen.error}</span>{/if}
      {#if room.notice}
        <span class={room.notice.error ? 'warn' : 'hint'}>{room.notice.text}</span>
        {#if room.notice.error}<button class="dismiss" title="Dismiss" onclick={() => (room.notice = null)}>×</button>{/if}
      {/if}
    </footer>
  </div>
{/if}

<style>
  .gate { max-width: 30em; margin: 15vh auto; padding: 0 16px; text-align: center; }
  .app {
    height: 100%;
    display: grid;
    /* Wide: the rooms down the left with the mixer for the one you are
       in under it, then what is yours alone, then the room's
       conversation, the last two the same size. The rooms get only
       what the widest fader needs, which is not much now that a strip
       is three tight lines. */
    grid-template-columns: 260px 1fr 1fr;
    grid-template-rows: auto 1fr auto;
    grid-template-areas: "head head head" "rooms cards log" "foot foot foot";
  }
  /* With nothing in your cards there is no column for them, and the
     conversation has the half they were taking. */
  .app.nocards {
    grid-template-columns: 260px 1fr;
    grid-template-areas: "head head" "rooms log" "foot foot";
  }
  :global(.rooms) { grid-area: rooms; border-right: 1px solid var(--line); }
  :global(.cards) { grid-area: cards; border-right: 1px solid var(--line); }
  /* One area at a time is a narrow-screen idea; here all three show. */
  .views { display: none; }
  header {
    grid-area: head;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  h1 { font-size: 17px; margin: 0; }
  /* The game the room is playing, beside the room's own name. */
  .playing {
    font-size: 13px;
    font-weight: 600;
    padding: 3px 10px;
    border-radius: 999px;
    background: var(--agent);
    color: var(--accent-ink);
  }
  h2 { font-size: 13px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); margin: 0 0 4px; }
  .spacer { flex: 1; }
  .offline { color: var(--danger); font-size: 13px; }
  :global(.log) { grid-area: log; }
  :global(.board) { grid-area: board; }
  .app.watching { grid-template-areas: "head head head" "rooms board board" "foot foot foot"; }
  .app.nocards.watching { grid-template-areas: "head head" "rooms board" "foot foot"; }
  .app.watching :global(.log), .app.watching :global(.cards) { display: none; }
  footer button.on { background: var(--accent); color: var(--accent-ink); border-color: var(--accent); }
  .ask.peek { width: min(46em, calc(100vw - 32px)); max-width: none; }
  .peek-title { font-size: 17px; text-transform: none; letter-spacing: 0; color: inherit; margin: 0 0 4px; }
  .peek-summary { color: var(--muted); margin: 0 0 12px !important; }
  .peek-prompt {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 60vh;
    overflow-y: auto;
    background: var(--bubble);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 12px 14px;
    margin-bottom: 14px;
    font-size: 14px;
    line-height: 1.5;
  }
  footer {
    grid-area: foot;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    border-top: 1px solid var(--line);
    background: var(--panel);
  }
  /* One width for every label it can have -- Mute, Unmute, Hold to
     talk, Talking... -- so the button under your hand is the same
     button it was a moment ago and nothing beside it moves. */
  .big { padding: 9px 18px; font-size: 15px; min-width: 12.5em; text-align: center; }
  .big.muted { background: var(--danger); color: #fff; border-color: var(--danger); }
  .big.ptt { touch-action: none; user-select: none; -webkit-user-select: none; }
  .big.ptt.talking { background: var(--ok); color: var(--accent-ink); border-color: var(--ok); }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; margin-left: 4px; }
  .vol { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--muted); }
  /* Device names run long; keep one from pushing the controls off. */
  /* The cog is an icon and nothing else, so it is square rather than a
     button with a picture in it. */
  .cog { display: inline-flex; align-items: center; justify-content: center; padding: 7px; line-height: 0; }
  .cog[aria-pressed='true'] { border-color: var(--accent); color: var(--accent); }
  .warn { color: var(--danger); font-size: 13px; }
  .dismiss { padding: 0 6px; line-height: 1.4; }
  .hear { width: 80px; height: 6px; border-radius: 3px; background: var(--line); overflow: hidden; }
  .hear span { display: block; height: 100%; background: var(--meter); transition: width 80ms linear; }
  .views { grid-area: views; gap: 0; padding: 0 16px 10px; }
  .views button {
    flex: 1;
    font-size: 13px;
    padding: 7px 6px;
    white-space: nowrap;
    border-radius: 0;
    border-width: 1px 0 1px 1px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
  }
  .views button:first-child { border-radius: 8px 0 0 8px; }
  .views button:last-child { border-radius: 0 8px 8px 0; border-right-width: 1px; }
  .views button.on { background: var(--accent); color: var(--accent-ink); border-color: var(--accent); }
  .views .badge {
    font-size: 11px;
    border-radius: 999px;
    padding: 0 6px;
    background: var(--agent);
    color: var(--accent-ink);
  }
  .views button.on .badge { background: var(--accent-ink); color: var(--accent); }

  /* Narrow: one area at a time, with the switch above it. */
  @media (max-width: 860px) {
    /* One column whether or not there are cards — the wide `.nocards`
       template is two classes, so it has to be put back by name here or
       its two columns survive into one. */
    .app, .app.nocards {
      grid-template-columns: 1fr;
      grid-template-rows: auto auto 1fr auto;
      grid-template-areas: "head" "views" "main" "foot";
    }
    .views { display: flex; }
    :global(.log), :global(.cards), :global(.rooms), :global(.board) { grid-area: main; min-height: 0; }
    /* Watching takes the whole area, and the switch above it goes with
       the areas it switched between: the board has its own way out.
       (The wide `.app.watching` template is two classes, so it must be
       put back here or its three columns survive into one.) */
    .app.watching, .app.nocards.watching { grid-template-areas: "head" "views" "main" "foot"; }
    .app.watching .views { display: none; }
    .app.watching :global(.log), .app.watching :global(.cards), .app.watching :global(.rooms) { display: none; }
    :global(.cards), :global(.rooms) { border-right: 0; }
    .app:not(.show-chat) :global(.log) { display: none; }
    .app:not(.show-cards) :global(.cards) { display: none; }
    .app:not(.show-room) :global(.rooms) { display: none; }
    header, footer { padding: 8px 16px; }
  }
</style>
