<script>
  import { room, closeProfile, avatarOf, theAIs } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';
  import { scaledDown } from '../lib/pixels.js';

  // How big the square on this card is, in one place: the CSS draws
  // it and `lib/pixels.js` is told it, and the two must agree.
  const FACE = 160;

  // What opens when a face is clicked: the picture at a size worth
  // looking at, and the few things the house actually knows about
  // whoever it belongs to. Not a form -- nothing here is edited from
  // this card. The bio is the AI's to write, and your own face is in
  // Settings.
  const p = $derived(room.profile);
  const src = $derived(p ? avatarOf(p.who) : null);
  const initials = $derived(
    (p?.who || '?')
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((w) => [...w][0])
      .join('')
      .toUpperCase(),
  );

  // Whether the picture is being made smaller rather than bigger --
  // see `lib/pixels.js`. A card opened on somebody else is a new
  // picture in the same square, so it is asked again each time.
  let smooth = $state(false);

  function onkeydown(e) {
    if (e.key === 'Escape') closeProfile();
  }
</script>

<svelte:window {onkeydown} />

{#if p}
  <!-- Click anywhere off it to close, which is what a card over a
       conversation is expected to do. The card itself stops the click
       so that selecting the bio doesn't dismiss it. -->
  <div
    class="shade"
    role="presentation"
    onclick={closeProfile}
  >
    <div
      class="who-card"
      role="dialog"
      aria-modal="true"
      aria-label="Who {p.who} is"
      style="--h: {hue(p.who)}"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.stopPropagation()}
      tabindex="-1"
    >
      <div class="face" class:agent={p.bot}>
        {#if src}
          <img {src} alt="" class:smooth onload={(e) => (smooth = scaledDown(e.currentTarget, FACE))} />
        {:else}
          <span class="initials">{initials}</span>
        {/if}
      </div>

      <h2>{p.who}</h2>
      <div class="tags">
        {#if p.puppet}
          <!-- Not an AI: a character one of them speaks through, with
               nothing of its own behind it. The one thing somebody
               clicking its face wants to know. -->
          <span class="tag ai" title="A character {theAIs()[0]?.name ?? 'the AI'} speaks through. There is nothing behind it: what it says was written for it.">a puppet</span>
        {:else if p.bot}
          <span class="tag ai">an AI</span>
        {:else if p.owner}
          <span class="tag">whose room this is</span>
        {/if}
        <!-- A person chose a language to read in; an AI just talks
             in the room's. Not the same claim, so not the same word. -->
        {#if p.language}
          <span class="tag">{p.bot ? 'speaks' : 'reads'} {p.language}</span>
        {/if}
        <span class="tag" class:here={p.here}>{p.here ? 'here' : 'away'}</span>
      </div>

      {#if p.loading}
        <p class="quiet">…</p>
      {:else if p.failed}
        <p class="quiet">The room couldn't say who this is.</p>
      {:else}
        {#if p.bio}
          <p class="bio">{p.bio}</p>
        {:else}
          {@const asker = theAIs().find((a) => a.name !== p.who)?.name}
          <p class="quiet">
            Nothing written about {p.bot ? 'it' : 'them'} yet.{#if asker}
              Ask {asker} out loud.{/if}
          </p>
        {/if}
      {/if}

      <button class="primary" onclick={closeProfile}>Done <kbd class="shortcut">Esc</kbd></button>
    </div>
  </div>
{/if}

<style>
  /* The sheet scrolls, not the card on it: a card that is its own
     scroller leaves the whole of the screen around it dead to the
     wheel, which on a tall face and a long description is most of
     what you are looking at. See the note in `app.css`. */
  .shade {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    display: grid;
    place-items: center;
    padding: 16px;
    overflow-y: auto;
    overscroll-behavior: contain;
    z-index: 50;
  }
  .who-card {
    background: var(--dialog-bg);
    border: 1px solid var(--line);
    border-radius: 14px;
    padding: 20px;
    width: min(26em, 100%);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    text-align: center;
    box-shadow: 0 10px 40px rgb(0 0 0 / 25%);
  }
  /* The same square as in the conversation, at a size worth looking
     at: a face is drawn to be seen big, and 36px is a label. */
  .face {
    /* `FACE` above is this number; they are read together. */
    width: 160px;
    height: 160px;
    border-radius: 12px;
    overflow: hidden;
    display: grid;
    place-items: center;
    background: var(--face-bg);
    color: var(--face-ink);
    border: 1px solid var(--face-line);
  }
  .face.agent {
    background: color-mix(in srgb, var(--agent) 14%, var(--bubble));
    color: var(--agent);
    border-color: color-mix(in srgb, var(--agent) 40%, var(--line));
  }
  .initials { font-size: 54px; font-weight: 650; user-select: none; }
  img {
    width: 100%;
    height: 100%;
    image-rendering: pixelated;
    object-fit: cover;
    display: block;
  }
  /* Bigger than the square, so it is being made smaller: a picture
     drawn at full resolution, or a photograph. Either wants
     smoothing. See `lib/pixels.js`. */
  img.smooth { image-rendering: auto; }
  h2 { margin: 0; font-size: 20px; }
  .tags { display: flex; flex-wrap: wrap; gap: 6px; justify-content: center; }
  .tag {
    font-size: 11px;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    padding: 2px 8px;
    border: 1px solid var(--line);
    border-radius: 999px;
    color: var(--muted);
  }
  .tag.here { color: var(--ok); border-color: color-mix(in srgb, var(--ok) 45%, var(--line)); }
  .tag.ai { color: var(--agent); border-color: color-mix(in srgb, var(--agent) 45%, var(--line)); }
  .bio { margin: 2px 0 0; line-height: 1.5; text-align: left; }
  .quiet { margin: 2px 0 0; color: var(--muted); }
  .who-card button { margin-top: 6px; }
</style>
