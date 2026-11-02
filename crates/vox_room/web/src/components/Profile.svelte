<script>
  import { room, closeProfile, avatarOf } from '../lib/room.svelte.js';
  import { hue } from '../lib/color.js';

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
          <img {src} alt="" />
        {:else}
          <span class="initials">{initials}</span>
        {/if}
      </div>

      <h2>{p.who}</h2>
      <div class="tags">
        {#if p.bot}
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
          {@const asker = room.agents.find((a) => a.name !== p.who)?.name}
          <p class="quiet">
            Nothing written about {p.bot ? 'it' : 'them'} yet.{#if asker}
              Ask {asker} out loud.{/if}
          </p>
        {/if}
        <!-- What the picture was asked for. Shown because it is the
             description they sat through an interview for, and because
             it is how anybody knows what to ask to have changed. -->
        {#if p.face}
          <p class="asked"><span>drawn as</span> {p.face}</p>
        {/if}
      {/if}

      <button class="primary" onclick={closeProfile}>Done <kbd>Esc</kbd></button>
    </div>
  </div>
{/if}

<style>
  .shade {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    display: grid;
    place-items: center;
    padding: 16px;
    z-index: 50;
  }
  .who-card {
    background: var(--bg, var(--bubble));
    border: 1px solid var(--line);
    border-radius: 14px;
    padding: 20px;
    width: min(26em, 100%);
    max-height: 90vh;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    text-align: center;
    box-shadow: 0 10px 40px rgb(0 0 0 / 25%);
  }
  /* The same square as in the conversation, at a size worth looking
     at: pixel art is drawn to be seen big, and 36px is a label. */
  .face {
    width: 160px;
    height: 160px;
    border-radius: 12px;
    overflow: hidden;
    display: grid;
    place-items: center;
    background: hsl(var(--h) 55% 92%);
    color: hsl(var(--h) 55% 32%);
    border: 1px solid hsl(var(--h) 40% 80%);
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
  .tag.here { color: hsl(140 45% 38%); border-color: hsl(140 35% 60%); }
  .tag.ai { color: var(--agent); border-color: color-mix(in srgb, var(--agent) 45%, var(--line)); }
  .bio { margin: 2px 0 0; line-height: 1.5; text-align: left; }
  .quiet { margin: 2px 0 0; color: var(--muted); }
  .asked {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
    text-align: left;
    align-self: stretch;
  }
  .asked span { text-transform: uppercase; font-size: 10px; letter-spacing: 0.05em; }
  .who-card button { margin-top: 6px; }
  @media (prefers-color-scheme: dark) {
    .face {
      background: hsl(var(--h) 35% 22%);
      color: hsl(var(--h) 45% 78%);
      border-color: hsl(var(--h) 30% 32%);
    }
    .tag.here { color: hsl(140 40% 62%); border-color: hsl(140 25% 38%); }
  }
</style>
