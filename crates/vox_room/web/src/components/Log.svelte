<script>
  import { room } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';

  let box;
  let stick = true;

  function onscroll() {
    stick = box.scrollHeight - box.scrollTop - box.clientHeight < 60;
  }

  $effect(() => {
    // Follow new text while the view is at the bottom.
    room.log.map((e) => e.text).join();
    if (stick && box) queueMicrotask(() => (box.scrollTop = box.scrollHeight));
  });

  const time = (ms) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
</script>

<div class="log" bind:this={box} {onscroll}>
  {#if room.log.length === 0}
    <p class="empty">
      Nothing said yet. Join the audio and start talking; say <strong>{room.agent.name}</strong>
      {room.mode === 'named' ? 'to bring it in' : 'or just talk, and it answers when you pause'}.
    </p>
  {/if}
  {#each room.log as e (e.id)}
    <div class="entry" class:agent={e.kind === 'agent'} class:you={e.who === room.you} style="--h: {hue(e.who)}">
      <div class="who">
        <span class="name">{e.who}</span>
        <span class="time">{time(e.at)}</span>
      </div>
      <div class="text" class:partial={e.partial} class:live={e.live}>
        {#if e.text}{e.text}{:else if e.kind === 'agent'}<span class="dots"><i></i><i></i><i></i></span>{/if}
      </div>
    </div>
  {/each}
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
  .dots { display: inline-flex; gap: 4px; padding: 4px 0; }
  .dots i { width: 6px; height: 6px; border-radius: 50%; background: var(--muted); animation: blink 1.2s infinite; }
  .dots i:nth-child(2) { animation-delay: 0.2s; }
  .dots i:nth-child(3) { animation-delay: 0.4s; }
  @keyframes blink { 0%, 80%, 100% { opacity: 0.25; } 40% { opacity: 1; } }
</style>
