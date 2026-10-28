<script>
  import { room, enterGame } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';

  let box;
  let stick = true;

  function onscroll() {
    stick = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  }

  $effect(() => {
    // Follow new text while the view is at the bottom.
    room.log.map((e) => e.text).join();
    room.agent.state;
    if (stick && box) queueMicrotask(() => (box.scrollTop = box.scrollHeight));
  });

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<div class="log" bind:this={box} {onscroll}>
  {#if room.log.length === 0}
    <p class="empty">
      {#if room.game}
        Entering <strong>{room.game}</strong>…
      {:else}
        Nothing said yet. Join the audio and start talking; say <strong>{room.agent.name}</strong>
        {room.mode === 'named' ? 'to bring it in' : 'or just talk, and it answers when you pause'}.
        Ask it to design a game with you, and it'll save one you can enter.
      {/if}
    </p>
  {/if}
  {#each room.log as e (e.id)}
    {#if e.kind === 'game'}
      {@const g = room.games.find((g) => g.name.toLowerCase() === e.text.toLowerCase())}
      <div class="entry agent game-entry">
        <div class="who"><span class="name">{e.who}</span><span class="time">{time(e.at)} · new game</span></div>
        <div class="card">
          <div class="title">{e.text}</div>
          {#if g}<p class="summary">{g.summary}</p>{/if}
          {#if !g}
            <span class="hint">This game was deleted.</span>
          {:else if room.game}
            <span class="hint">Go back to the lobby to play it.</span>
          {:else}
            <button class="primary" onclick={() => enterGame(g.name)}>Enter {g.name}</button>
          {/if}
        </div>
      </div>
    {:else}
    <div class="entry" class:agent={e.kind === 'agent'} class:you={e.who === room.you} style="--h: {hue(e.who)}">
      <div class="who">
        <span class="name">{e.who}</span>
        <span class="time">{time(e.at)}</span>
      </div>
      <div class="text" class:partial={e.partial} class:live={e.live}>
        {#if e.text}{e.text}{:else if e.kind === 'agent'}<span class="dots"><i></i><i></i><i></i></span>{/if}
      </div>
    </div>
    {/if}
  {/each}
  {#if room.agent.state === 'waiting'}
    <div class="entry agent waiting" aria-live="polite">
      <div class="who"><span class="name">{room.agent.name}</span><span class="time">waiting for a pause</span></div>
      <div class="text"><span class="dots"><i></i><i></i><i></i></span></div>
    </div>
  {/if}
</div>

<style>
  .log {
    overflow-y: auto;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
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
  .card .summary { margin: 0; color: var(--muted); }
  .hint { color: var(--muted); font-size: 13px; }
  .entry { max-width: min(42em, 92%); }
  .entry.you { align-self: flex-end; }
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
</style>
