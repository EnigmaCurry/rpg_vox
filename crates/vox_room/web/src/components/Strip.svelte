<script>
  import { command, room } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';
  import { screen, showThumb, streamOf, unwatch, watch } from '../lib/screen.svelte.js';

  // key: the strip's name in the mixer; label: what to show; doing: what
  // to say it is up to, for a strip that is nobody's microphone.
  let { key, label, online = true, mic = false, muted = false, speaking = false, agent = false, doing = undefined } = $props();

  const strip = $derived(room.strips[key] ?? { gain: 1, mute: false });
  const level = $derived(room.levels[key] ?? 0);
  // Meter in dB, -60..0 mapped to 0..100%.
  const pct = $derived(level > 0 ? Math.max(0, Math.min(100, ((20 * Math.log10(level) + 60) / 60) * 100)) : 0);
  const db = $derived(strip.gain > 0 ? `${(20 * Math.log10(strip.gain)).toFixed(1)} dB` : '-∞');

  function gain(e) {
    command({ type: 'strip', name: key, gain: Number(e.currentTarget.value) });
  }
  function mute() {
    command({ type: 'strip', name: key, mute: !strip.mute });
  }

  // The desktop this person is sharing, if they are. Whether it is open
  // full size is this tab's own business: clicking here opens it for
  // nobody but you.
  const stream = $derived(agent ? undefined : streamOf(label));
  const open = $derived(stream && screen.watching === stream.id);

  let thumb = $state();
  $effect(() => (thumb && stream ? showThumb(stream.id, thumb) : undefined));
</script>

<div class="strip" class:offline={!online} class:speaking style="--h: {agent ? 270 : hue(label)}">
  <div class="top">
    <span class="dot" title={online ? 'here' : 'away'}></span>
    <span class="label">{label}{#if label === room.you}<em>&nbsp;(you)</em>{/if}</span>
    <span class="state">{doing ?? (agent ? room.agent.state : speaking ? 'talking' : mic && muted ? 'muted' : mic ? 'mic on' : online ? 'no mic' : 'away')}</span>
  </div>
  {#if stream}
    <button class="desktop" class:open onclick={() => (open ? unwatch() : watch(stream.id))}
      title={open ? `Close ${label}'s desktop` : `Open ${label}'s desktop, for you alone`}>
      <canvas bind:this={thumb} width={stream.thumb.w} height={stream.thumb.h}></canvas>
      <span class="what">{open ? 'Close' : 'Open'}</span>
    </button>
  {/if}
  <div class="meter"><div class="fill" style="width: {pct}%"></div></div>
  <div class="controls">
    <input type="range" min="0" max="2" step="0.01" value={strip.gain} oninput={gain}
      ondblclick={() => command({ type: 'strip', name: key, gain: 1 })} aria-label="{label} level" />
    <span class="db">{db}</span>
    <button class:on={strip.mute} onclick={mute} aria-pressed={strip.mute}
      title={strip.mute
        ? `Unmute ${label} for everyone`
        : agent
          ? `Mute ${label} for everyone`
          : `Mute ${label} for everyone (and stop transcribing them)`}>{strip.mute ? 'Muted' : 'Mute'}</button>
  </div>
</div>

<style>
  .strip { border: 1px solid var(--line); border-radius: 10px; padding: 10px 12px; background: var(--panel); }
  .strip.speaking { border-color: hsl(var(--h) 60% 55%); box-shadow: 0 0 0 2px hsl(var(--h) 60% 55% / 25%); }
  .strip.offline { opacity: 0.55; }
  .top { display: flex; align-items: center; gap: 8px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); }
  .strip:not(.offline) .dot { background: var(--ok); }
  .label { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .label em { font-weight: 400; color: var(--muted); font-style: normal; }
  .state { font-size: 12px; color: var(--muted); }
  /* The desktop they are sharing, under their name. A button, because
     the only thing it does is open the picture on this screen. */
  .desktop {
    display: block;
    position: relative;
    width: 100%;
    margin-top: 8px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 6px;
    overflow: hidden;
    background: #000;
    line-height: 0;
    cursor: pointer;
  }
  .desktop:hover, .desktop:focus-visible { border-color: hsl(var(--h) 60% 55%); }
  .desktop.open { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  /* As wide as the strip, but never tall enough to push the level and
     the mute off a narrow screen, where the strip is the whole width. */
  .desktop canvas { display: block; width: 100%; height: auto; max-height: 160px; object-fit: contain; }
  .desktop .what {
    position: absolute;
    right: 4px;
    bottom: 4px;
    font-size: 11px;
    line-height: 1.6;
    padding: 0 6px;
    border-radius: 4px;
    background: rgb(0 0 0 / 60%);
    color: #fff;
    opacity: 0;
    transition: opacity 120ms;
  }
  .desktop:hover .what, .desktop:focus-visible .what, .desktop.open .what { opacity: 1; }
  @media (prefers-reduced-motion: reduce) { .desktop .what { transition: none; } }
  .meter { height: 6px; border-radius: 3px; background: var(--line); margin: 8px 0; overflow: hidden; }
  .fill { height: 100%; background: var(--meter); transition: width 80ms linear; }
  .controls { display: flex; align-items: center; gap: 8px; }
  .controls input { flex: 1; min-width: 0; }
  .db { font-size: 12px; color: var(--muted); width: 4.5em; text-align: right; font-variant-numeric: tabular-nums; }
  .controls button { padding: 2px 10px; font-size: 13px; }
  .controls button.on { background: var(--danger); color: #fff; border-color: var(--danger); }
</style>
