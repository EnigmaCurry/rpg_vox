<script>
  import { command, room } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';
  import { screen, showThumb, streamOf, unwatch, watch } from '../lib/screen.svelte.js';
  import Face from './Face.svelte';

  // One channel of the room's mixer: who it is, how loud they are
  // coming in, and the fader and mute everyone shares.
  //
  // Three tight lines and no card around them. The strips live in the
  // room list now, under the room they belong to, and a panel of boxed
  // cards there costs the rooms below it more height than the boxes
  // were worth: the names already read as a list, and the branch line
  // beside them already says what they belong to.
  //
  // key: where this channel's meter arrives under; label: what to show;
  // doing: what to say it is up to, for a strip that is nobody's
  // microphone.
  //
  // `fader` and `onfader` are for a channel that isn't one of the
  // room's — the interpreter, which is yours and not shared. Left out,
  // the fader is the room's and changing it changes it for everyone,
  // which is what a mixer is for.
  let {
    key,
    label,
    mic = false,
    muted = false,
    speaking = false,
    agent = false,
    doing = undefined,
    fader = undefined,
    onfader = undefined,
    children = undefined,
    // Whether this channel is somebody -- a person or an AI -- and so
    // has a face to show beside the name. The interpreter is a channel
    // and not anybody, and has none.
    somebody = false,
  } = $props();

  const strip = $derived(fader ?? room.strips[key] ?? { gain: 1, mute: false });
  const change = (what) =>
    onfader ? onfader(what) : command({ type: 'strip', name: key, ...what });
  const level = $derived(room.levels[key] ?? 0);
  // Meter in dB, -60..0 mapped to 0..100%.
  const pct = $derived(level > 0 ? Math.max(0, Math.min(100, ((20 * Math.log10(level) + 60) / 60) * 100)) : 0);
  const db = $derived(strip.gain > 0 ? `${(20 * Math.log10(strip.gain)).toFixed(1)} dB` : '-∞');
  // The reading is only shown once the fader has been moved off unity.
  // At rest it is a number saying nothing, in a column this narrow; the
  // moment you drag it, it is the thing you are looking for, and it
  // appears under your thumb.
  const moved = $derived(Math.abs(strip.gain - 1) > 0.005);
  // Nothing of theirs is reaching the room: at the door, mic shut, or
  // muted. They are here, and you can't hear them.
  const quiet = $derived(!agent && (!mic || muted || strip.mute));

  const state = $derived(
    doing ?? (speaking ? 'talking' : mic && muted ? 'muted' : mic ? 'mic on' : 'no mic'),
  );

  function gain(e) {
    change({ gain: Number(e.currentTarget.value) });
  }
  function mute() {
    change({ mute: !strip.mute });
  }

  // The desktop this person is sharing, if they are. Whether it is open
  // full size is this tab's own business: clicking here opens it for
  // nobody but you.
  const stream = $derived(agent ? undefined : streamOf(label));
  const open = $derived(stream && screen.watching === stream.id);

  let thumb = $state();
  $effect(() => (thumb && stream ? showThumb(stream.id, thumb) : undefined));
</script>

<div class="strip" class:speaking class:quiet style="--h: {agent ? 270 : hue(label)}">
  <div class="top">
    {#if somebody}
      <!-- The same square as in the conversation, small: this is the
           list of who is here, and a name is easier to find by face. -->
      <Face who={label} {agent} size={22} />
    {:else}
      <span class="dot"></span>
    {/if}
    <span class="label">{label}{#if label === room.you}<em>&nbsp;(you)</em>{/if}</span>
    <span class="state">{state}</span>
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
      ondblclick={() => change({ gain: 1 })}
      title="{label} level — double-click to put it back" aria-label="{label} level" />
    {#if moved}<span class="db">{db}</span>{/if}
    <button class:on={strip.mute} onclick={mute} aria-pressed={strip.mute}
      title={onfader
        ? `${strip.mute ? 'Unmute' : 'Mute'} ${label} for yourself — nobody else's hearing changes`
        : strip.mute
        ? `Unmute ${label} for everyone`
        : agent
          ? `Mute ${label} for everyone`
          : `Mute ${label} for everyone (and stop transcribing them)`}>{strip.mute ? 'Muted' : 'Mute'}</button>
  </div>
  <!-- Anything that belongs to this channel rather than to the room:
       when the AI speaks, under the AI. -->
  {#if children}<div class="its-own">{@render children()}</div>{/if}
</div>

<style>
  .strip { padding: 4px 4px 5px; border-radius: 7px; }
  /* Somebody you can't hear: at the door, mic shut, or muted. They are
     in the room all the same, so they are dimmed rather than moved or
     dropped. */
  .strip.quiet { opacity: 0.6; }
  /* Talking: the row itself lifts, since there is no border left to
     colour. Enough to catch the eye in a list, not enough to flicker. */
  .strip.speaking {
    opacity: 1;
    background: color-mix(in srgb, hsl(var(--h) 60% 55%) 16%, transparent);
  }
  .top { display: flex; align-items: center; gap: 7px; font-size: 14px; line-height: 1.25; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--muted); flex: none; }
  .strip:not(.quiet) .dot { background: var(--ok); }
  .strip.speaking .dot { background: hsl(var(--h) 60% 45%); }
  .label { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .label em { font-weight: 400; color: var(--muted); font-style: normal; }
  .state { font-size: 11px; color: var(--muted); flex: none; }
  /* The desktop they are sharing, under their name. A button, because
     the only thing it does is open the picture on this screen. */
  .desktop {
    display: block;
    position: relative;
    width: 100%;
    margin-top: 6px;
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
  .meter { height: 3px; border-radius: 2px; background: var(--line); margin: 4px 0; overflow: hidden; }
  .fill { height: 100%; background: var(--meter); transition: width 80ms linear; }
  .controls { display: flex; align-items: center; gap: 7px; }
  .controls input { flex: 1; min-width: 0; height: 14px; margin: 0; }
  .db { font-size: 11px; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .controls button { padding: 1px 8px; font-size: 12px; border-radius: 6px; }
  .controls button.on { background: var(--danger); color: #fff; border-color: var(--danger); }
  .its-own { margin-top: 6px; }
</style>
