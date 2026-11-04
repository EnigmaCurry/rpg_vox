<script>
  import { onMount } from 'svelte';
  import { room, command, ask, answer, whoami, leaveAudio, setMuted, setPushToTalk, setBots, talk, busyAI, dropPicture, notice, mutedByRoom, theAI, theAIs } from './lib/room.svelte.js';
  import { screen, share, stopSharing, supported as canShare } from './lib/screen.svelte.js';
  import Log from './components/Log.svelte';
  import Cards from './components/Cards.svelte';
  import Enter from './components/Enter.svelte';
  import Board from './components/Board.svelte';
  import Settings from './components/Settings.svelte';
  import Profile from './components/Profile.svelte';
  import People from './components/People.svelte';
  import Rooms from './components/Rooms.svelte';
  import Say from './components/Say.svelte';

  // Narrow screens show one area at a time; wide ones show all three.
  let view = $state('chat');

  // Nothing dealt to you, no area for it: the column (and its tab) are
  // gone rather than sitting there empty, and the conversation takes the
  // room back. It comes and goes with the cards, since the AI can take
  // the last one back at any moment — in which case whoever was looking
  // at that tab is put back on the conversation.
  const hasCards = $derived(room.cards.length > 0);
  // The room's mixer has your own strip turned off, so nothing you say
  // here is heard however open your mic is. Said by the mic button and
  // nowhere else: a dead button named for the mute that stopped it is
  // the whole message, and a line of prose beside it only repeats what
  // the button already is.
  const stripMuted = $derived(mutedByRoom());
  const area = $derived(view === 'cards' && !hasCards ? 'chat' : view);

  // The rooms are not a tab any more -- which room you are in is the
  // heading over all of these, and every other room is behind it -- so
  // what is left in that area is who is in this one and how loud they
  // are, which is what the tab is called.
  const views = $derived([
    { id: 'chat', label: 'Conversation' },
    ...(hasCards ? [{ id: 'cards', label: 'Just for you' }] : []),
    { id: 'room', label: 'People' },
  ]);

  // Only who this browser is. The sockets wait for Join Audio: a tab at
  // the door is not in the room, and nothing of the room's is its
  // business until it is.
  onMount(whoami);

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

  // A picture dragged in from outside goes into the conversation. The
  // target is the window and not the conversation column, or even the
  // room: the picture lands in the conversation wherever it is let go
  // of, and a target you have to find is one that makes you aim at a
  // wall of text while holding a file.
  //
  // The window and not the room's own div for a second reason. A file
  // let go of over a page that has not claimed the drag is *opened*
  // by the browser, which here means the room is gone and the audio
  // with it -- so the drag is claimed everywhere this app is on
  // screen, including the door, and whether there is anywhere to put
  // it is answered afterwards, in words. (It also keeps a handler for
  // something nobody can do with a keyboard off an element that is
  // only a layout: a drop is reachable by pointer alone, and Ctrl+V
  // below is the way in that isn't.)
  //
  // Whether the room is the thing on the screen at all: the door and
  // the pages of their own have no conversation to put a picture in.
  const inRoom = $derived(room.audio === 'on' && !room.settings && !room.managing);
  // And only into the conversation the room is having. One it has
  // finished with has nowhere to put a picture, which is the same
  // reason there is nothing to type into it -- see `Say`.
  const canDrop = $derived(inRoom && !room.reading);
  // What to say when there isn't. Both ways in say it, and they say
  // the same thing because the same thing is wrong with both. Only
  // where it can be read, though: the footer it goes in is the room's.
  const refuse = () => inRoom && notice(OVER);
  const OVER = 'That conversation is over; come back to the one the room is having.';
  // Whether something is over the window right now, counted rather
  // than flagged: `dragleave` fires as the pointer crosses into a
  // child, and a flag turns the overlay off halfway across the room.
  let dragging = $state(0);
  // A drag of anything else -- a link, selected text, a tab -- is not
  // ours to take: the browser's own handling of it is better than an
  // overlay that says it will do something it won't.
  const hasFiles = (e) => [...(e.dataTransfer?.types ?? [])].includes('Files');
  function dragenter(e) {
    if (canDrop && hasFiles(e)) dragging += 1;
  }
  function dragover(e) {
    if (!hasFiles(e)) return;
    // Needed on every dragover and not only on the drop: without it
    // the drop never reaches the page at all.
    e.preventDefault();
    e.dataTransfer.dropEffect = canDrop ? 'copy' : 'none';
  }
  function drop(e) {
    dragging = 0;
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (canDrop) dropPicture(e.dataTransfer.files);
    else refuse();
  }

  // Ctrl+V, which is how a screenshot gets anywhere: the clipboard
  // hands it over as a file, so this is the drop with no drag in
  // front of it.
  //
  // Only a clipboard that is *only* files, though. Copying part of a
  // web page puts the text and its pictures on the clipboard
  // together, and that is a paste meant for whatever has the cursor
  // -- taking the pictures out of it and posting them to the room
  // would be answering a different question. A screenshot, or a file
  // copied in a file manager, carries no text with it.
  function paste(e) {
    if (!inRoom) return;
    const files = [...(e.clipboardData?.files ?? [])];
    if (!files.length || e.clipboardData.types.includes('text/plain')) return;
    e.preventDefault();
    if (canDrop) dropPicture(files, 'pasted');
    else refuse();
  }

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
    // Muted on the room's mixer, the space bar still belongs to your
    // microphone -- it just has nothing left to do with it. Claimed and
    // ignored rather than handed back to the page, so nothing jumps
    // under you, and your own mute is left where it was: changing it
    // would move a switch that cannot make you heard.
    if (stripMuted) return;
    if (room.pushToTalk) talk(true);
    else if (!e.repeat) setMuted(!room.muted);
  }
  function onkeyup(e) {
    if (e.code !== 'Space' || !room.pushToTalk) return;
    e.preventDefault();
    talk(false);
  }
</script>

<!-- Both ways a picture comes in are on the window. A screenshot is
     pasted wherever the cursor happens to be, including the box you
     type into, and it is meant for the room either way; and a drag
     the page hasn't claimed by the time it is let go of takes the
     page with it, so it is claimed wherever it lands. -->
<svelte:window
  {onkeydown}
  {onkeyup}
  onblur={() => talk(false)}
  onpaste={paste}
  ondragenter={dragenter}
  ondragover={dragover}
  ondragleave={() => (dragging = Math.max(0, dragging - 1))}
  ondrop={drop}
/>

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

<!-- Over whatever is showing. A face is clickable from the
     conversation and from the people list, and who somebody is is the
     same card either way. -->
<Profile />

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
    <!-- What is about to happen, while a file is over the room: the
         one thing a drop can do here, said before it is let go of. -->
    {#if dragging > 0}
      <div class="dropping" aria-hidden="true">
        <span>Drop a picture into the conversation</span>
      </div>
    {/if}
    <!-- No title bar. What a room is called is the heading of the room
         list, which is the column it belongs to; what a conversation
         is called, and what is done to it, is the bar over the
         conversation; and the two live controls that were up here are
         in the footer with the rest of what you reach for. -->
    <nav class="views" aria-label="Which area to show">
      {#each views as v (v.id)}
        <button aria-pressed={area === v.id} class:on={area === v.id} onclick={() => (view = v.id)}>
          {v.label}
          {#if v.id === 'cards'}<span class="badge">{room.cards.length}</span>{/if}
        </button>
      {/each}
    </nav>

    <!-- What you do with the room, over the people in it: sharing a
         desktop with them, walking out, and what the room gave you to
         speak into. Icons, because the footer below is for the one
         button you aim at without looking and these are not it. -->
    {#if room.audio === 'on'}
      <div class="tools">
        {#if canShare()}
          <button class="share" class:on={screen.sharing} aria-label={screen.sharing ? 'Stop sharing' : 'Share screen'}
            onclick={() => (screen.sharing ? stopSharing() : share())}
            title={screen.sharing
              ? 'Stop sharing your desktop with the room.'
              : 'Share a window or screen. Everyone sees a thumbnail of it in the mixer and can open it for themselves.'}>
            📺️
          </button>
        {/if}
        <button class="leave" aria-label="Leave audio" onclick={leaveAudio}
          title="Leave the audio and go back to the door.">🚪</button>
        <button class="cog" aria-label="Settings" aria-pressed={room.settings}
          onclick={() => (room.settings = !room.settings)}
          title="Your name, the app's volume, and what the room gave you to speak into.">
          <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" focusable="false">
            <path fill="currentColor" d="M19.14 12.94a7.07 7.07 0 0 0 0-1.88l2.03-1.58a.5.5 0 0 0 .12-.64l-1.92-3.32a.5.5 0 0 0-.6-.22l-2.39.96a7.03 7.03 0 0 0-1.63-.94l-.36-2.54a.5.5 0 0 0-.5-.42h-3.84a.5.5 0 0 0-.49.42l-.36 2.54c-.59.24-1.13.56-1.63.94l-2.39-.96a.5.5 0 0 0-.6.22L2.66 8.84a.5.5 0 0 0 .12.64l2.03 1.58a7.07 7.07 0 0 0 0 1.88L2.78 14.52a.5.5 0 0 0-.12.64l1.92 3.32c.13.22.39.31.6.22l2.39-.96c.5.38 1.04.7 1.63.94l.36 2.54c.04.24.25.42.49.42h3.84c.25 0 .46-.18.5-.42l.36-2.54c.59-.24 1.13-.56 1.63-.94l2.39.96c.22.09.47 0 .6-.22l1.92-3.32a.5.5 0 0 0-.12-.64l-2.03-1.58ZM12 15.5A3.5 3.5 0 1 1 15.5 12 3.5 3.5 0 0 1 12 15.5Z" />
          </svg>
        </button>
      </div>
    {/if}

    <Rooms />

    {#if hasCards}<Cards />{/if}

    <Log />

    {#if screen.watching != null}<Board />{/if}

    <footer class="panel">
      {#if room.audio === 'on'}
        <div class="mic">
        {#if room.pushToTalk}
          <!-- `hushed` is the room's own sound being off while you
               hold, which is what push to talk is for somebody on
               speakers: see `RoomAudio.hushRoom`. On a headset it is
               nothing you want, so it is a setting and the title says
               which way it is set -- it does not change the button's
               size, since the one under your hand must stay where it
               is. -->
          <button class="big ptt" class:talking={room.talking} class:hushed={room.hushed}
            disabled={!room.mic || stripMuted}
            title={room.pttHush
              ? "While you hold this, the room's own sound is off, so your microphone hears only you. Settings has the other way round."
              : 'While you hold this, you go on hearing the room — which is what a headset wants. Settings can cut it instead, for speakers.'}
            onpointerdown={(e) => { e.currentTarget.setPointerCapture(e.pointerId); talk(true); }}
            onpointerup={() => talk(false)} onpointercancel={() => talk(false)}
            onkeydown={(e) => { if (e.code === 'Enter') { e.preventDefault(); talk(true); } }}
            onkeyup={(e) => { if (e.code === 'Enter') { e.preventDefault(); talk(false); } }}>
            {stripMuted ? 'Muted on the mixer' : room.talking ? 'Talking…' : 'Hold to talk'} <kbd>Space</kbd>
          </button>
        {:else}
          <button class="big" class:muted={room.muted || stripMuted} onclick={() => setMuted(!room.muted)} disabled={!room.mic || stripMuted}>
            {stripMuted ? 'Muted on the mixer' : room.muted ? 'Unmute' : 'Mute'} <kbd>Space</kbd>
          </button>
        {/if}
        <!-- The two switches that say what the room does with your
             microphone, stacked beside the button they are about and
             half its height each, so the pair is exactly as tall as it
             is and nothing moves when either changes. Both are yours
             alone, both are set here rather than in Settings because
             they are reached mid-conversation, and both have a nuance
             on the settings page: what the off position means. -->
        <div class="switches">
          <button class="switch ptt-switch" class:on={room.pushToTalk}
            aria-pressed={room.pushToTalk}
            onclick={() => setPushToTalk(!room.pushToTalk)}
            title={room.pushToTalk
              ? (room.pttHush
                ? 'Push to talk is on: your mic opens only while you hold Space or the talk button, and the room goes quiet while you hold it. Off, your mic is simply open.'
                : 'Push to talk is on: your mic opens only while you hold Space or the talk button, and you go on hearing the room while you talk. Off, your mic is simply open.')
              : 'Your mic is open and everybody hears everybody. Turn this on and it opens only while you hold Space or the talk button — Settings says what holding it does to the room’s own sound.'}>
            PTT
          </button>
          <!-- Whether the AIs may answer you, which is yours and
               nobody else's: it changes nothing for anybody else in
               the room. -->
          <button class="switch bots-switch" class:on={room.bots === 'any'}
            aria-pressed={room.bots === 'any'}
            onclick={() => setBots(room.bots !== 'any')}
            title={room.bots === 'any'
              ? `${theAI('or')} may answer what you say. Turn this off and ${theAIs().length === 1 ? 'it stays' : 'they stay'} out of it — Settings says whether your calling ${theAIs().length === 1 ? 'it' : 'one of them'} by name still gets an answer.`
              : room.bots === 'named'
              ? `${theAI('or')} stays out of what you say unless you say its name. Nobody else is affected.`
              : `${theAI('or')} answers nothing of yours, not even its own name. Nobody else is affected.`}>
            BOTS
          </button>
        </div>
        </div>
        <!-- Your mic on the left, and what you do with the room on the
             right: everything you set once is behind the cog. A footer
             strung with sliders and readouts put the way out next to a
             volume control, and none of it was anything you touched
             twice. -->
        {#if busyAI()}
          <!-- Whichever of them is talking: Stop cuts the reply that is
               in flight, and in a room with two AIs only one is. It is
               here because it is the other thing you reach for without
               looking, and it is only here while there is a reply to
               cut. -->
          <button class="danger" onclick={() => command({ type: 'stop' })}>Stop {busyAI().name}</button>
        {/if}
        <!-- Saying it and typing it, side by side. -->
        <Say />
        {#if !room.connected}<span class="offline">reconnecting…</span>{/if}
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
    /* Wide: which room you are in at the top left -- the heading of the
       page, there being no bar above it -- with who is in that room
       under it, then what is yours alone, then the room's
       conversation, the last two the same size. The left column gets
       only what the widest fader needs, which is not much now that a
       strip is three tight lines. */
    grid-template-columns: 260px 1fr 1fr;
    grid-template-rows: auto auto 1fr auto;
    grid-template-areas:
      "pick  cards log"
      "tools cards log"
      "rooms cards log"
      "foot  foot  foot";
    /* What a framed theme uses to hold the panels off the edges of the
       window and off each other; zero in a flat one, where the columns
       meet along the hairlines below and the room runs to the glass. */
    padding: var(--app-pad);
    gap: var(--app-gap);
  }
  /* With nothing in your cards there is no column for them, and the
     conversation has the half they were taking. */
  .app.nocards {
    grid-template-columns: 260px 1fr;
    grid-template-areas: "pick log" "tools log" "rooms log" "foot foot";
  }
  /* The heading and the strips are two cells of one column, so the
     rule down its edge is on both -- and the heading's own rule under
     it is its, in the component. */
  :global(.pick), :global(.rooms) { border-right: var(--column-rule); }
  :global(.pick) { grid-area: pick; }
  :global(.rooms) { grid-area: rooms; min-height: 0; }
  /* What you do with the room, over the people in it. */
  .tools {
    grid-area: tools;
    display: flex;
    /* Against the right edge, under the caret that opens the rooms:
       the far side of this strip is where what-you-do-with-the-room
       lives, and the room's name has the near side to itself. */
    justify-content: flex-end;
    gap: 6px;
    padding: 8px 10px;
    border-right: var(--column-rule);
    border-bottom: var(--column-rule);
  }
  .tools button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 2.2em;
    padding: 5px 0;
    font-size: 15px;
    line-height: 1;
  }
  .tools button.on {
    background: var(--btn-on-image) var(--btn-on-bg);
    color: var(--btn-on-ink);
    border-color: var(--btn-on-border);
    box-shadow: var(--btn-on-shadow);
  }
  .tools .cog[aria-pressed='true'] { border-color: var(--accent); color: var(--accent); }
  .tools .leave:hover { border-color: var(--danger); }
  :global(.cards) { grid-area: cards; border-right: var(--column-rule); }
  /* One area at a time is a narrow-screen idea; here all three show. */
  .views { display: none; }
  h2 { font-size: 13px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); margin: 0 0 4px; }
  .spacer { flex: 1; }
  .offline { color: var(--danger); font-size: 13px; }
  :global(.log) { grid-area: log; }
  :global(.board) { grid-area: board; }
  .app.watching {
    grid-template-areas:
      "pick  board board"
      "tools board board"
      "rooms board board"
      "foot  foot  foot";
  }
  .app.nocards.watching {
    grid-template-areas: "pick board" "tools board" "rooms board" "foot foot";
  }
  .app.watching :global(.log), .app.watching :global(.cards) { display: none; }
  /* Over everything while a file is on its way in. Fixed rather than
     absolute, so it covers the room without being a cell of the grid
     it is a child of -- and it never takes the pointer, because the
     drop belongs to the room underneath it. */
  .dropping {
    position: fixed;
    inset: 0;
    z-index: 5;
    pointer-events: none;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    outline: 2px dashed var(--accent);
    outline-offset: -10px;
  }
  .dropping span {
    background: var(--bg);
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 10px;
    padding: 10px 16px;
    font-weight: 600;
  }
  .ask.peek { width: min(46em, calc(100vw - 32px)); max-width: none; }
  .peek-title { font-size: 17px; text-transform: none; letter-spacing: 0; color: inherit; margin: 0 0 4px; }
  .peek-summary { color: var(--muted); margin: 0 0 12px !important; }
  .peek-prompt {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 60vh;
    overflow-y: auto;
    overscroll-behavior: contain;
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
    /* Lines stack upward, so a box with no room to sit beside the talk
       button goes above it rather than below: the thing you are typing
       into is nearest the conversation it is going into, and the button
       under your thumb stays where it was. */
    flex-wrap: wrap-reverse;
    /* The box you type into grows; everything beside it stays on the
       line the two of them share. Reversed wrapping turns the cross
       axis over with it, so the foot of the line is `start` here. */
    align-items: flex-start;
    gap: 8px;
    padding: 8px 12px;
    border-top: var(--column-rule);
    /* A panel that is also the thing you reach for, so it names its own
       fill: in a flat theme it is raised off the page, in a framed one
       it is another panel of the console. The `.panel` class on it
       draws the frame out of this. */
    --surface: var(--foot-surface);
  }
  /* One width for every label it can have -- Mute, Unmute, Hold to
     talk, Talking... -- so the button under your hand is the same
     button it was a moment ago and nothing beside it moves. */
  /* The one button you aim at without looking, so it is the one shaped
     like something to press: a deeper bevel than the switches beside
     it, and far deeper than the box you type in, which stays flat. A
     flat theme leaves `--key-*` at the ordinary button's values and
     this is the same button it always was, only wider. */
  .big {
    padding: 9px 18px;
    font-size: 15px;
    min-width: 12.5em;
    text-align: center;
    background: var(--cut-control-edges), var(--key-image) var(--btn-bg);
    box-shadow: var(--key-shadow);
  }
  .big:hover { background: var(--cut-control-edges), var(--key-image) var(--btn-hover-bg); }
  /* Down: the face sinks and the light moves to the bottom edge. */
  .big:active:not(:disabled), .big.talking { box-shadow: var(--key-down-shadow); }
  .big.muted {
    background: var(--danger);
    color: var(--on-solid);
    border-color: var(--danger);
    box-shadow: var(--key-down-shadow);
  }
  .big.ptt { touch-action: none; user-select: none; -webkit-user-select: none; }
  .big.ptt.talking {
    background: var(--ok);
    color: var(--on-solid);
    border-color: var(--ok);
  }
  /* The button and the switch that says what it does, as one thing:
     the box beside them grows upward, and these stay on its last line
     rather than being pulled apart by it. */
  .mic { display: flex; align-items: center; gap: 8px; }
  /* Two rows the height of the one button beside them: the gap between
     them is the gap `.mic` already uses, so the three edges line up. */
  .switches { display: flex; flex-direction: column; gap: 3px; }
  .switch {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.08em;
    line-height: 1;
    padding: 5px 9px;
    min-width: 4.2em;
    color: var(--muted);
  }
  /* On is on, and reads as on without being read: these are the two
     switches nobody looks at twice. */
  .switch.on {
    background: var(--btn-on-image) var(--btn-on-bg);
    color: var(--btn-on-ink);
    border-color: var(--btn-on-border);
    box-shadow: var(--btn-on-shadow);
  }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; margin-left: 4px; }
  .vol { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--muted); }
  /* Device names run long; keep one from pushing the controls off. */
  .warn { color: var(--danger); font-size: 13px; }
  .dismiss { padding: 0 6px; line-height: 1.4; }
  .hear { width: 80px; height: 6px; border-radius: 3px; background: var(--line); overflow: hidden; }
  .hear span { display: block; height: 100%; background: var(--meter); transition: width 80ms linear; }
  .views { grid-area: views; gap: 0; padding: 0 12px 6px; }
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
  .views button.on {
    background: var(--btn-on-image) var(--btn-on-bg);
    color: var(--btn-on-ink);
    border-color: var(--btn-on-border);
  }
  .views .badge {
    font-size: 11px;
    border-radius: 999px;
    padding: 0 6px;
    background: var(--agent);
    color: var(--on-solid);
  }
  .views button.on .badge { background: var(--btn-on-ink); color: var(--btn-on-bg); }

  /* Narrow: one area at a time, with the switch above it. */
  @media (max-width: 860px) {
    /* One column whether or not there are cards — the wide `.nocards`
       template is two classes, so it has to be put back by name here or
       its two columns survive into one. */
    .app, .app.nocards {
      grid-template-columns: 1fr;
      grid-template-rows: auto auto auto 1fr auto;
      grid-template-areas: "pick" "tools" "views" "main" "foot";
    }
    .views { display: flex; }
    /* The heading stays out of the tabs and over them: which room you
       are in is true of whichever area you are looking at, and on a
       narrow screen it is the only thing saying so. */
    :global(.pick) { border-right: 0; }
    .tools { border-right: 0; }
    :global(.log), :global(.cards), :global(.rooms), :global(.board) { grid-area: main; min-height: 0; }
    /* Watching takes the whole area, and the switch above it goes with
       the areas it switched between: the board has its own way out.
       (The wide `.app.watching` template is two classes, so it must be
       put back here or its three columns survive into one.) */
    .app.watching, .app.nocards.watching { grid-template-areas: "pick" "tools" "views" "main" "foot"; }
    .app.watching .views { display: none; }
    .app.watching :global(.log), .app.watching :global(.cards), .app.watching :global(.rooms) { display: none; }
    :global(.cards), :global(.rooms) { border-right: 0; }
    .app:not(.show-chat) :global(.log) { display: none; }
    .app:not(.show-cards) :global(.cards) { display: none; }
    .app:not(.show-room) :global(.rooms) { display: none; }
    footer { padding: 5px 10px; }
  }
</style>
