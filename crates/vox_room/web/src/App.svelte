<script>
  import { onMount } from 'svelte';
  import { room, connect, command, enterGame, peekGame, ask, answer, whoami, leaveAudio, setMuted, setVolume, setPushToTalk, talk } from './lib/room.svelte.js';
  import Log from './components/Log.svelte';
  import Strip from './components/Strip.svelte';
  import Cards from './components/Cards.svelte';
  import Enter from './components/Enter.svelte';

  onMount(async () => {
    if (await whoami()) connect();
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

  async function deleteGame(name) {
    if (await ask(`Delete ${name} for good?`, 'Delete')) command({ type: 'delete_game', name });
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

{#if room.denied}
  <main class="gate">
    <h1>Voice room</h1>
    <p>You need an invite link to come in. Ask whoever runs the room for one.</p>
  </main>
{:else}
  <!-- Until the audio is joined the room is behind the door: visible,
       but not to be clicked, tabbed into, or read out. -->
  <div class="app" inert={room.audio !== 'on'}>
    <header>
      <h1>{room.game ?? 'Voice room'}</h1>
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
      <button onclick={newConversation}>{room.game ? 'Restart game' : 'New conversation'}</button>
      {#if !room.connected}<span class="offline">reconnecting…</span>{/if}
    </header>

    <Log />

    <aside>
      <Cards />

      <h2>In the room</h2>
      {#each room.people as p (p.name)}
        <Strip key={p.name} label={p.name} online={p.online} mic={p.mic} muted={p.muted} speaking={p.speaking} />
      {/each}
      <Strip key={room.agent.strip} label={room.agent.name} agent />
      <p class="hint">Levels and mutes here change the room for everyone, and a muted person isn't transcribed. You never hear yourself.</p>

      <h2 class="games-head">Games</h2>
      {#if room.games.length === 0}
        <p class="hint">None yet. In the lobby, ask {room.agent.name} to design one with you.</p>
      {/if}
      <ul class="games">
        {#each room.games as g (g.name)}
          <li class:current={room.game === g.name}>
            <div class="gname">
              <span>{g.name}</span>
              {#if room.game === g.name}<span class="tag">playing</span>{/if}
              {#if g.settings?.reply && g.settings.reply !== 'room'}
                <span class="tag mode" title={modes.find((m) => m.mode === g.settings.reply)?.hint}>
                  {modes.find((m) => m.mode === g.settings.reply)?.label}
                </span>
              {/if}
            </div>
            <p class="gsum">{g.summary}</p>
            {#if room.game !== g.name}
              <div class="gact">
                {#if !room.game}<button onclick={() => enterGame(g.name)}>Play</button>{/if}
                <button onclick={() => peekGame(g.name)} title="Show the game's hidden prompt, to you only">Peek</button>
                <button class="danger" onclick={() => deleteGame(g.name)}>Delete</button>
              </div>
            {:else}
              <div class="gact">
                <button onclick={() => peekGame(g.name)} title="Show the game's hidden prompt, to you only">Peek</button>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    </aside>

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
        <button onclick={leaveAudio} title="Leave the audio and go back to the door.">Leave audio</button>
      {/if}
      {#if room.audioError}<span class="warn">Mic: {room.audioError}</span>{/if}
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
    grid-template-columns: 1fr 300px;
    grid-template-rows: auto 1fr auto;
    grid-template-areas: "head head" "log side" "foot foot";
  }
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
  .ask { border: 1px solid var(--line); border-radius: 12px; padding: 18px 20px; max-width: min(28em, calc(100vw - 32px)); background: var(--bg, var(--bubble)); color: inherit; }
  .ask::backdrop { background: rgb(0 0 0 / 35%); }
  .ask p { margin: 0 0 14px; }
  .ask-actions { display: flex; justify-content: flex-end; align-items: center; gap: 8px; }
  .ask-actions .hint { margin-right: auto; }
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
  .games-head { margin-top: 10px; }
  .games { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  .games li { border: 1px solid var(--line); border-radius: 10px; padding: 8px 10px; background: var(--bubble); }
  .games li.current { border-color: var(--agent); }
  .gname { display: flex; align-items: center; gap: 6px; font-weight: 600; }
  .tag.mode { color: var(--muted); font-weight: 400; }
  .tag { font-size: 11px; font-weight: 500; color: var(--agent); border: 1px solid currentColor; border-radius: 999px; padding: 0 6px; }
  .gsum { margin: 4px 0 6px; font-size: 13px; color: var(--muted); }
  .gact { display: flex; gap: 6px; }
  .gact button { padding: 2px 10px; font-size: 13px; }
  aside {
    grid-area: side;
    border-left: 1px solid var(--line);
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    overflow-y: auto;
  }
  .hint { font-size: 12px; color: var(--muted); margin: 0; }
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
  @media (max-width: 760px) {
    .app {
      grid-template-columns: 1fr;
      grid-template-rows: auto 1fr auto auto;
      grid-template-areas: "head" "log" "side" "foot";
    }
    aside { border-left: 0; border-top: 1px solid var(--line); max-height: 35vh; }
    header, footer { padding: 8px 16px; }
  }
</style>
