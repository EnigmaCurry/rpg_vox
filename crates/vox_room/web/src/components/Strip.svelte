<script>
  import { command, room, theAI, theAIs } from '../lib/room.svelte.js';
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
    // Switched off in this room, for a voice that can be. `undefined`
    // is a channel with no such switch -- a person, a puppet, the
    // interpreter -- and then there is no button for one.
    off = undefined,
    // Whether this person is taking answers from the AIs: their own
    // BOTS switch, which is on the roster everybody is sent. Only a
    // person has one -- an AI is the other side of that question, and
    // the interpreter is not anybody -- so it is left out for those.
    bots = undefined,
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
  // muted. They are here, and you can't hear them. A voice switched
  // off is the same thing for the same reason -- it is in the list and
  // it is not in the room's conversation -- and dimming is what says
  // so without being read.
  const quiet = $derived(off === true || (!agent && (!mic || muted || strip.mute)));

  const state = $derived(
    doing ?? (speaking ? 'talking' : mic && muted ? 'muted' : mic ? 'mic on' : 'no mic'),
  );

  // The AIs being off for this person, which is only ever worth saying
  // when they are: everybody starts with the switch on, and a mark
  // against every name in the room would be a mark that means nothing.
  // So the icon is the exception and its absence is the rule.
  const botsOff = $derived(bots && bots !== 'any' ? bots : null);
  const yours = $derived(label === room.you);
  const one = $derived(theAIs().length === 1);
  const why = $derived(
    !botsOff
      ? ''
      : yours
        ? botsOff === 'never'
          ? `You have BOTS off all the way: nothing you say reaches ${theAI()}, its own name included. Nobody else in the room is affected.`
          : `You have BOTS off: ${theAI()} ${one ? 'stays' : 'stay'} out of what you say until you say its name. Nobody else in the room is affected.`
        : botsOff === 'never'
          ? `${label} has BOTS off all the way: nothing they say reaches ${theAI()}, its own name included. Everybody else is answered as usual.`
          : `${label} has BOTS off: ${theAI()} ${one ? 'stays' : 'stay'} out of what they say until they say its name. Everybody else is answered as usual.`,
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
  // A voice the room made has no level worth setting: it is as loud
  // as the room asked it to be, and nobody has ever wanted it louder
  // or quieter than the rest of the room. So the fader is not shown
  // for one -- what is left is the two switches that do mean
  // something, side by side. The interpreter is the exception and
  // keeps its own, because turning that one down is how you turn it
  // off for yourself: see `Rooms`.
  const fades = $derived(!agent || onfader !== undefined);

  const stream = $derived(agent ? undefined : streamOf(label));
  const open = $derived(stream && screen.watching === stream.id);

  let thumb = $state();
  $effect(() => (thumb && stream ? showThumb(stream.id, thumb) : undefined));
</script>

<div class="strip card-chrome" class:speaking class:quiet style="--h: {agent ? 270 : hue(label)}">
  <div class="top">
    {#if somebody}
      <!-- The same square as in the conversation, small: this is the
           list of who is here, and a name is easier to find by face. -->
      <Face who={label} {agent} size={22} />
    {:else}
      <span class="dot"></span>
    {/if}
    <span class="label">{label}{#if yours}<em>&nbsp;(you)</em>{/if}</span>
    <!-- Not taking answers. The same mark for both ways of being off,
         because at this size the thing to see is whether the AIs are
         in what somebody says at all; which of the two it is is the
         nuance, and it is in the title and in how solid the mark is. -->
    {#if botsOff}
      <span class="bots" class:all={botsOff === 'never'} role="img" title={why} aria-label={why}>
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
          <path d="M3 2.5h10A1.5 1.5 0 0 1 14.5 4v4.5A1.5 1.5 0 0 1 13 10H7.5L4.75 12.25V10H3A1.5 1.5 0 0 1 1.5 8.5V4A1.5 1.5 0 0 1 3 2.5Z"
            fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" />
          <!-- Struck through, and heavier than the bubble: at this size
               the thing that has to read is the crossing out. -->
          <path d="M2.6 13.4 13.4 2.6" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" />
        </svg>
      </span>
    {/if}
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
    {#if fades}
      <!-- `--at` is what paints the groove behind the slug: a range the
           page has styled has no way of its own to say how far along it
           is. Two is the top of this fader. -->
      <input type="range" min="0" max="2" step="0.01" value={strip.gain} oninput={gain}
        style="--at: {Math.round((strip.gain / 2) * 100)}%"
        ondblclick={() => change({ gain: 1 })}
        title="{label} level — double-click to put it back" aria-label="{label} level" />
      {#if moved}<span class="db">{db}</span>{/if}
    {:else}
      <span class="spacer"></span>
    {/if}
    <!-- Off is off: nothing is asked of it, so nothing is thought and
         nothing is said. It keeps its place here and everything set on
         it, which is what makes this a switch rather than a way of
         getting rid of a voice. -->
    {#if off !== undefined}
      <button class="power" class:on={!off} aria-pressed={!off}
        onclick={() => command({ type: 'bot_off', bot: label, off: !off })}
        title={off
          ? `${label} is switched off in this room: it is asked nothing and says nothing. Press to bring it back.`
          : `Switch ${label} off in this room. It keeps its place and everything set on it, and is asked nothing until it is switched back on — nobody else's room changes.`}>
        {off ? 'Off' : 'On'}
      </button>
    {/if}
    <button class="mute" class:on={strip.mute} onclick={mute} aria-pressed={strip.mute}
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
  /* A fader is a card: in a framed theme a small panel of its own,
     in a flat one four pixels of padding and nothing else. Which is
     why there is no background or border named here -- `card-chrome`
     has them, out of the theme. */
  .strip { padding: var(--card-pad, 4px 4px 5px); }
  /* Somebody you can't hear: at the door, mic shut, or muted. They are
     in the room all the same, so they are dimmed rather than moved or
     dropped. */
  .strip.quiet { opacity: 0.6; }
  /* Talking: the row itself lifts, since there is no border left to
     colour. Enough to catch the eye in a list, not enough to flicker. */
  .strip.speaking {
    opacity: 1;
    /* The colour only: the card's own fill, bevel and cut corners are
       the theme's and must survive somebody opening their mouth. */
    background-color: color-mix(in srgb, var(--person-edge) 16%, transparent);
    box-shadow: var(--card-edge), inset 0 0 0 1px color-mix(in srgb, var(--person-edge) 45%, transparent);
  }
  .top { display: flex; align-items: center; gap: 7px; font-size: 14px; line-height: 1.25; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--muted); flex: none; }
  .strip:not(.quiet) .dot { background: var(--ok); }
  .strip.speaking .dot { background: var(--person-edge); }
  .label { font-weight: 600; flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .label em { font-weight: 400; color: var(--muted); font-style: normal; }
  .state { font-size: 11px; color: var(--muted); flex: none; }
  /* The AIs are out of what this person says. In the room's own colour
     for an AI, since that is what it is about. */
  .bots {
    flex: none;
    display: inline-flex;
    color: var(--agent);
    /* Off unless they are named is halfway off, and reads that way.
       All the way off is the solid mark. */
    opacity: 0.5;
  }
  .bots.all { opacity: 1; }
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
  .desktop:hover, .desktop:focus-visible { border-color: var(--person-edge); }
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
    color: #ffffff;
    opacity: 0;
    transition: opacity 120ms;
  }
  .desktop:hover .what, .desktop:focus-visible .what, .desktop.open .what { opacity: 1; }
  @media (prefers-reduced-motion: reduce) { .desktop .what { transition: none; } }
  .meter { height: 3px; border-radius: 2px; background: var(--line); margin: 4px 0; overflow: hidden; }
  .fill { height: 100%; background: var(--meter); transition: width 80ms linear; }
  .controls { display: flex; align-items: center; gap: 7px; }
  .controls input { flex: 1; min-width: 0; height: 14px; margin: 0; }
  /* What the fader was taking, for a voice that has none: the
     switches stay over on the right where they are on every other
     strip, rather than sliding under the name. */
  .controls .spacer { flex: 1; }
  /* On is the quiet state here, unlike every other switch in the room:
     a voice being in the room is the ordinary thing, and a button lit
     up to say so against every bot in the list would be the list
     shouting. Off is the one worth seeing -- and it is outlined rather
     than filled, because filled red next to it is the mute, and the
     two are different things. The strip dims as well, which is the
     part you see without reading. */
  .controls button.power.on { color: var(--muted); }
  .controls button.power:not(.on) { color: var(--danger); border-color: var(--danger); }
  .db { font-size: 11px; color: var(--muted); font-variant-numeric: tabular-nums; white-space: nowrap; }
  .controls button { padding: 1px 8px; font-size: 12px; border-radius: 6px; }
  /* Named, not just `.on`: there is a second switch on this row now,
     and a rule that said "any switch in here that is on is red" put
     the mute's colour on it. */
  .controls button.mute.on { background: var(--danger); color: var(--on-solid); border-color: var(--danger); }
  .its-own { margin-top: 6px; }
</style>
