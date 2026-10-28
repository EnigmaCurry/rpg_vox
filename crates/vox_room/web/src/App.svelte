<script>
  import { onMount } from 'svelte';
  import { room, connect, command, whoami, joinAudio, leaveAudio, setMuted, setVolume, setNoHeadset } from './lib/room.svelte.js';
  import { supported } from './lib/audio.js';
  import Log from './components/Log.svelte';
  import Strip from './components/Strip.svelte';

  onMount(async () => {
    if (await whoami()) connect();
  });

  const agentLabel = $derived(
    { listening: 'listening', thinking: 'thinking…', speaking: 'speaking' }[room.agent.state] ?? room.agent.state,
  );

  function newConversation() {
    if (confirm('Start a new conversation? The current one is kept in the database but cleared from the screen.')) {
      command({ type: 'new_conversation' });
    }
  }

  // Space toggles your mute, unless you're typing somewhere.
  function onkeydown(e) {
    if (e.code !== 'Space' || room.audio !== 'on' || e.target.closest('input, textarea, button')) return;
    e.preventDefault();
    setMuted(!room.muted);
  }
</script>

<svelte:window {onkeydown} />

{#if room.denied}
  <main class="gate">
    <h1>Voice room</h1>
    <p>You need an invite link to come in. Ask whoever runs the room for one.</p>
  </main>
{:else}
  <div class="app">
    <header>
      <h1>Voice room</h1>
      <span class="pill {room.agent.state}" aria-live="polite">{room.agent.name} is {agentLabel}</span>
      {#if room.agent.state !== 'listening'}
        <button class="danger" onclick={() => command({ type: 'stop' })}>Stop {room.agent.name}</button>
      {/if}
      <div class="spacer"></div>
      <div class="seg" role="radiogroup" aria-label="When {room.agent.name} answers">
        <button role="radio" aria-checked={room.mode === 'named'} class:on={room.mode === 'named'}
          onclick={() => command({ type: 'mode', mode: 'named' })}>When named</button>
        <button role="radio" aria-checked={room.mode === 'always'} class:on={room.mode === 'always'}
          onclick={() => command({ type: 'mode', mode: 'always' })}>After every pause</button>
      </div>
      <button onclick={newConversation}>New conversation</button>
      {#if !room.connected}<span class="offline">reconnecting…</span>{/if}
    </header>

    <Log />

    <aside>
      <h2>In the room</h2>
      {#each room.people as p (p.name)}
        <Strip key={p.name} label={p.name} online={p.online} mic={p.mic} muted={p.muted} speaking={p.speaking} />
      {/each}
      <Strip key={room.agent.strip} label={room.agent.name} agent />
      <p class="hint">Levels and mutes here change the room for everyone, and a muted person isn't transcribed. You never hear yourself.</p>
    </aside>

    <footer>
      {#if !supported()}
        <span class="warn">This browser can't do the room's audio (it needs WebCodecs: a recent Chrome, Firefox or Safari).</span>
      {:else if room.audio === 'off' || room.audio === 'error'}
        <button class="primary big" onclick={joinAudio}>Join audio</button>
        <span class="hint">Best with a headset: anyone can interrupt {room.agent.name} just by talking.</span>
        <label class="vol">
          <input type="checkbox" checked={room.noHeadset} onchange={(e) => setNoHeadset(e.currentTarget.checked)} />
          No headset
        </label>
      {:else}
        <button class="big" class:muted={room.muted} onclick={() => setMuted(!room.muted)} disabled={!room.mic}>
          {room.muted ? 'Unmute' : 'Mute'} <kbd>Space</kbd>
        </button>
        <span class="vol">You hear</span>
        <span class="hear" title="What you hear: everyone but you">
          <span style="width: {room.hearing > 0 ? Math.max(0, Math.min(100, ((20 * Math.log10(room.hearing) + 60) / 60) * 100)) : 0}%"></span>
        </span>
        <label class="vol">Your volume
          <input type="range" min="0" max="1.5" step="0.01" value={room.volume}
            oninput={(e) => setVolume(Number(e.currentTarget.value))} />
        </label>
        <label class="vol" title="Playing through speakers? This has the browser keep the room's sound out of your mic, so {room.agent.name} doesn't hear itself.">
          <input type="checkbox" checked={room.noHeadset} onchange={(e) => setNoHeadset(e.currentTarget.checked)} />
          No headset
        </label>
        <button onclick={leaveAudio}>Leave audio</button>
        {#if room.audio === 'starting'}<span class="hint">starting…</span>{/if}
      {/if}
      {#if room.audioError}<span class="warn">Mic: {room.audioError}</span>{/if}
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
  .pill { font-size: 13px; padding: 3px 10px; border-radius: 999px; background: var(--bubble); color: var(--muted); }
  .pill.thinking { color: var(--agent); }
  .pill.speaking { background: var(--agent); color: var(--accent-ink); }
  .seg { display: inline-flex; border: 1px solid var(--line); border-radius: 8px; overflow: hidden; }
  .seg button { border: 0; border-radius: 0; padding: 5px 10px; }
  .seg button.on { background: var(--accent); color: var(--accent-ink); }
  .offline { color: var(--danger); font-size: 13px; }
  :global(.log) { grid-area: log; }
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
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; margin-left: 4px; }
  .vol { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--muted); }
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
