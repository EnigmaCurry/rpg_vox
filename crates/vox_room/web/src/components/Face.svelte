<script>
  import { avatarOf, openProfile } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';
  import { scaledDown } from '../lib/pixels.js';

  // `who` is the name the room calls them, which is what a line in the
  // conversation carries and what a face is looked up by. `agent` is
  // whether it is one of the AIs, which have their own colour rather
  // than one worked out from the name. `opens` is whether clicking it
  // opens who they are -- true wherever a face stands for somebody,
  // and false where one is already being looked at.
  // `size` is how big the square is drawn. 36 beside a line in the
  // conversation; smaller in the list of who is here, where it sits
  // on one line of 14px text.
  let { who, agent = false, opens = true, size = 36 } = $props();

  const src = $derived(avatarOf(who));

  // Enough of a name to tell two people apart at this size: the first
  // letter of the first two words.
  const initials = $derived(
    (who || '?')
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((w) => [...w][0])
      .join('')
      .toUpperCase(),
  );

  // A picture the browser couldn't load, by address. The roster said
  // there was one, so something has gone wrong between then and now --
  // a face taken away mid-session, most likely. Showing the square
  // their name already has is the right answer either way, and it is
  // remembered by address so a *new* picture is tried afresh.
  let broken = $state({});
  const show = $derived(src && !broken[src]);

  // Whether this face is being made smaller rather than bigger, which
  // decides how it is drawn -- see `lib/pixels.js`. Answered again for
  // every picture that loads here, because a new face can be a
  // photograph where the one before it was a sprite.
  let smooth = $state(false);
</script>

<!-- The square beside everything somebody says: their picture, or the
     tint their name already has with their initials in it. Never
     nothing -- the column has to line up whether or not a face has
     been made, so the placeholder is the same size and shape as the
     picture it is standing in for. -->
<svelte:element
  this={opens ? 'button' : 'div'}
  class="face"
  class:agent
  class:opens
  style="--h: {hue(who)}; --size: {size}px; --initials: {Math.round(size * 0.36)}px"
  title={opens ? `Who ${who} is` : who}
  type={opens ? 'button' : undefined}
  aria-hidden={opens ? undefined : 'true'}
  onclick={opens ? () => openProfile(who) : undefined}
>
  {#if show}
    <img
      {src}
      alt=""
      class:smooth
      onload={(e) => (smooth = scaledDown(e.currentTarget, size))}
      onerror={() => (broken = { ...broken, [src]: true })}
    />
  {:else}
    <span class="initials">{initials}</span>
  {/if}
</svelte:element>

<style>
  .face {
    /* Square, and the same square whichever branch above drew it. It
       does not shrink: the conversation is a column of names that line
       up, and a face that changed width per person would unalign it. */
    flex: 0 0 auto;
    width: var(--size);
    height: var(--size);
    border-radius: max(4px, calc(var(--size) / 4.5));
    overflow: hidden;
    display: grid;
    place-items: center;
    background: hsl(var(--h) 55% 92%);
    color: hsl(var(--h) 55% 32%);
    border: 1px solid hsl(var(--h) 40% 80%);
    /* No margin of its own: where it sits is the business of whatever
       is placing it. In the conversation it lines up with the name
       above the bubble; in the list of who is here it is on the line. */
  }
  .face.agent {
    background: color-mix(in srgb, var(--agent) 14%, var(--bubble));
    color: var(--agent);
    border-color: color-mix(in srgb, var(--agent) 40%, var(--line));
  }
  /* It is a button where it opens somebody, so it has to be told not
     to look like one: the square is the whole of the control. */
  .face.opens { padding: 0; cursor: pointer; }
  .face.opens:hover { border-color: hsl(var(--h) 45% 55%); }
  .face.opens:focus-visible { outline: 2px solid hsl(var(--h) 55% 50%); outline-offset: 2px; }
  .face.agent.opens:hover { border-color: var(--agent); }
  .initials {
    font-size: var(--initials);
    font-weight: 650;
    letter-spacing: 0.02em;
    user-select: none;
  }
  img {
    width: 100%;
    height: 100%;
    /* Pixel art, blown up to the square it is shown in: nearest
       neighbour is the whole point of the form, and bilinear would
       turn it to mush. */
    image-rendering: pixelated;
    object-fit: cover;
    display: block;
  }
  /* A picture bigger than the square -- a photograph somebody dropped
     into the room, most likely. It is being made smaller, and nearest
     neighbour throws away most of a photograph on the way down. */
  img.smooth { image-rendering: auto; }
  @media (prefers-color-scheme: dark) {
    .face {
      background: hsl(var(--h) 35% 22%);
      color: hsl(var(--h) 45% 78%);
      border-color: hsl(var(--h) 30% 32%);
    }
  }
</style>
