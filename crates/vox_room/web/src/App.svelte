<script>
  import { onMount } from 'svelte';
  import { room, open, command, ask, answer, whoami, leaveAudio, setMuted, setVolume, setPushToTalk, talk } from './lib/room.svelte.js';
  import { screen, share, stopSharing, supported as canShare } from './lib/screen.svelte.js';
  import Log from './components/Log.svelte';
  import Cards from './components/Cards.svelte';
  import Enter from './components/Enter.svelte';
  import Games from './components/Games.svelte';
  import Board from './components/Board.svelte';
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

  onMount(async () => {
    if (await whoami()) open();
  });

  const agentLabel = $derived(
    {
      listening: 'listening',
      waiting: 'about to answer',
      thinking: 'thinking',
      speaking: 'speaking',
    }[room.agent.state] ?? room.agent.state,
  );

  const modes = $derived([
    {
      mode: 'named',
      label: 'When named',
      hint: `${room.agent.name} says nothing until someone says its name.`,
    },
    {
      mode: 'auto',
      label: 'When it fits',
      hint: `${room.agent.name} is shown every pause and decides for itself whether the moment is its own, so it can sit in a conversation without answering every gap.`,
    },
    {
      mode: 'always',
      label: 'Every pause',
      hint: `${room.agent.name} answers whenever the room goes quiet, with no judgment about whose turn it is.`,
    },
  ]);

  // Why the mode is what it is, when the room and not a click decided it.
  const modeHint = $derived(
    room.locked
      ? `${room.game} decides when ${room.agent.name} answers, so this can't be changed until you go back to the lobby.`
      : room.engaged
        ? `You said its name, so ${room.agent.name} has the turn at every pause — judging each one — until the room has been quiet a while. Say "thanks" or "goodbye" and it goes back to waiting for its name.`
        : `When ${room.agent.name} speaks.`,
  );

  async function newConversation() {
    const question = room.game
      ? `Restart ${room.game} from the beginning, for everyone?`
      : 'Start a new conversation? The current one is kept in the database but cleared from the screen.';
    if (await ask(question, room.game ? 'Restart' : 'New conversation')) command({ type: 'new_conversation' });
  }

  async function lobby() {
    if (await ask(`Leave ${room.game} and take everyone back to the lobby?`, 'Back to lobby')) command({ type: 'lobby' });
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
  // unless you're typing somewhere.
  function spaceKey(e) {
    return e.code === 'Space' && room.audio === 'on' && !e.target.closest('input, textarea, button, dialog');
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
{:else}
  <!-- Until the audio is joined the room is behind the door: visible,
       but not to be clicked, tabbed into, or read out. -->
  <div class="app show-{area}" class:watching={screen.watching != null} class:nocards={!hasCards}
    inert={room.audio !== 'on'}>
    <header>
      <!-- Which room you are in, or the game it has become. The list of
           rooms is down the left; this is where you are. -->
      <h1>{room.game ?? room.title}</h1>
      {#if room.game}<button onclick={lobby}>Back to lobby</button>{/if}
      <span class="pill {room.agent.state}" aria-live="polite">
        {#if room.agent.state === 'thinking' || room.agent.state === 'waiting'}
          <span class="dots" aria-hidden="true"><i></i><i></i><i></i></span>
        {:else if room.agent.state === 'speaking'}
          <span class="bars" aria-hidden="true"><i></i><i></i><i></i><i></i></span>
        {/if}
        {room.agent.name} is {agentLabel}
      </span>
      {#if room.agent.state === 'thinking' || room.agent.state === 'speaking'}
        <button class="danger" onclick={() => command({ type: 'stop' })}>Stop {room.agent.name}</button>
      {/if}
      <div class="spacer"></div>
      <div class="seg" class:locked={room.locked} role="radiogroup" title={modeHint}
        aria-label="When {room.agent.name} speaks">
        {#each modes as m (m.mode)}
          <button role="radio" aria-checked={room.mode === m.mode} class:on={room.mode === m.mode}
            disabled={room.locked} title={m.hint}
            onclick={() => command({ type: 'mode', mode: m.mode })}>
            {m.label}{#if room.engaged && room.mode === m.mode}<span class="dot" aria-hidden="true"></span>{/if}
          </button>
        {/each}
      </div>
      <Games />
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
            onkeydown={(e) => { if (e.code === 'Space' || e.code === 'Enter') { e.preventDefault(); talk(true); } }}
            onkeyup={(e) => { if (e.code === 'Space' || e.code === 'Enter') { e.preventDefault(); talk(false); } }}>
            {room.talking ? 'Talking…' : 'Hold to talk'} <kbd>Space</kbd>
          </button>
        {:else}
          <button class="big" class:muted={room.muted} onclick={() => setMuted(!room.muted)} disabled={!room.mic}>
            {room.muted ? 'Unmute' : 'Mute'} <kbd>Space</kbd>
          </button>
        {/if}
        <label class="vol" title="Your mic stays muted except while you hold Space or the talk button.">
          <input type="checkbox" checked={room.pushToTalk} onchange={(e) => { setPushToTalk(e.currentTarget.checked); e.currentTarget.blur(); }} />
          Push to talk
        </label>
        {#if room.micName}
          <span class="vol mic-name" title="The microphone the browser gave the room: {room.micName}">
            Mic: {room.micName}
          </span>
        {/if}
        <span class="vol">You hear</span>
        <span class="hear" title="What you hear: everyone but you">
          <span style="width: {room.hearing > 0 ? Math.max(0, Math.min(100, ((20 * Math.log10(room.hearing) + 60) / 60) * 100)) : 0}%"></span>
        </span>
        <label class="vol">Your volume
          <input type="range" min="0" max="1.5" step="0.01" value={room.volume}
            oninput={(e) => setVolume(Number(e.currentTarget.value))} />
        </label>
        {#if canShare()}
          <button class:on={screen.sharing} onclick={() => (screen.sharing ? stopSharing() : share())}
            title={screen.sharing
              ? 'Stop sharing your desktop with the room.'
              : 'Share a window or screen. Everyone sees a thumbnail of it in the mixer and can open it for themselves.'}>
            {screen.sharing ? 'Stop sharing' : 'Share screen'}
          </button>
        {/if}
        <button onclick={leaveAudio} title="Leave the audio and go back to the door.">Leave audio</button>
      {/if}
      {#if room.audioError}<span class="warn">Mic: {room.audioError}</span>{/if}
      {#if screen.error}<span class="warn">Screen: {screen.error}</span>{/if}
      {#if room.notice}
        <span class={room.notice.error ? 'warn' : 'hint'}>{room.notice.text}</span>
        {#if room.notice.error}<button class="dismiss" title="Dismiss" onclick={() => (room.notice = null)}>×</button>{/if}
      {/if}
    </footer>
  </div>

  {#if room.audio !== 'on'}<Enter />{/if}
{/if}

<style>
  .gate { max-width: 30em; margin: 15vh auto; padding: 0 16px; text-align: center; }
  .app {
    height: 100%;
    display: grid;
    /* Wide: the rooms down the left with the mixer for the one you are
       in under it, then what is yours alone, then the room's
       conversation, the last two the same size. */
    grid-template-columns: 300px 1fr 1fr;
    grid-template-rows: auto 1fr auto;
    grid-template-areas: "head head head" "rooms cards log" "foot foot foot";
  }
  /* With nothing in your cards there is no column for them, and the
     conversation has the half they were taking. */
  .app.nocards {
    grid-template-columns: 300px 1fr;
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
  h2 { font-size: 13px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted); margin: 0 0 4px; }
  .spacer { flex: 1; }
  .pill { font-size: 13px; padding: 3px 10px; border-radius: 999px; background: var(--bubble); color: var(--muted); display: inline-flex; align-items: center; gap: 6px; }
  .pill.waiting { color: var(--agent); }
  .pill.thinking { color: var(--agent); box-shadow: 0 0 0 1px var(--agent); animation: glow 1.6s ease-in-out infinite; }
  .pill.speaking { background: var(--agent); color: var(--accent-ink); }
  .pill .dots { display: inline-flex; gap: 3px; }
  .pill .dots i { width: 5px; height: 5px; border-radius: 50%; background: currentColor; animation: hop 1s ease-in-out infinite; }
  .pill.waiting .dots i { animation-duration: 1.8s; opacity: 0.6; }
  .pill .dots i:nth-child(2) { animation-delay: 0.15s; }
  .pill .dots i:nth-child(3) { animation-delay: 0.3s; }
  .pill .bars { display: inline-flex; align-items: center; gap: 2px; height: 12px; }
  .pill .bars i { width: 3px; height: 100%; border-radius: 2px; background: currentColor; transform-origin: center; animation: talk 0.9s ease-in-out infinite; }
  .pill .bars i:nth-child(2) { animation-delay: 0.2s; }
  .pill .bars i:nth-child(3) { animation-delay: 0.45s; }
  .pill .bars i:nth-child(4) { animation-delay: 0.1s; }
  @keyframes hop { 0%, 60%, 100% { transform: translateY(0); } 30% { transform: translateY(-3px); } }
  @keyframes talk { 0%, 100% { transform: scaleY(0.3); } 50% { transform: scaleY(1); } }
  @keyframes glow { 0%, 100% { box-shadow: 0 0 0 1px var(--agent); } 50% { box-shadow: 0 0 0 3px color-mix(in srgb, var(--agent) 35%, transparent); } }
  @media (prefers-reduced-motion: reduce) { .pill *, .pill { animation: none !important; } }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .seg button { border: 0; border-radius: 0; padding: 5px 10px; white-space: nowrap; }
  .seg button.on { background: var(--accent); color: var(--accent-ink); }
  .seg.locked { opacity: 0.75; }
  .seg button:disabled { cursor: default; }
  /* A back-and-forth the room started, rather than a mode anyone picked. */
  .seg .dot {
    display: inline-block;
    width: 6px;
    height: 6px;
    margin-left: 6px;
    border-radius: 50%;
    background: currentColor;
    animation: pulse 2s ease-in-out infinite;
  }
  @keyframes pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 1; } }
  @media (prefers-reduced-motion: reduce) { .seg .dot { animation: none; } }
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
  .big { padding: 9px 18px; font-size: 15px; }
  .big.muted { background: var(--danger); color: #fff; border-color: var(--danger); }
  .big.ptt { touch-action: none; user-select: none; -webkit-user-select: none; }
  .big.ptt.talking { background: var(--ok); color: var(--accent-ink); border-color: var(--ok); }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; margin-left: 4px; }
  .vol { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--muted); }
  /* Device names run long; keep one from pushing the controls off. */
  .mic-name { max-width: 22ch; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; display: inline-block; }
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
