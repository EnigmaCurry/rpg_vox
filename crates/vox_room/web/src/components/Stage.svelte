<script>
  import { thePuppets } from '../lib/room.svelte.js';
  import Face from './Face.svelte';

  // The stage: the puppets in the room, standing where everybody can
  // see them, with whoever is speaking lit.
  //
  // It is here because a puppet show is the one thing in this room that
  // is *watched* rather than read. Everything else the room does is a
  // column of text and a row of faders, and that is right for a
  // conversation -- but a scene played by three characters is a scene,
  // and following it in a scrolling transcript means reading the name
  // on each line to work out who is talking. On the stage you look at
  // it: the one speaking is lit, and the others are standing there
  // waiting their turn.
  //
  // Faces at 80 pixels, against 36 beside a line in the conversation.
  // Big enough to read a character's face as a face rather than as an
  // icon of one, which is the whole difference between a cast list and
  // a stage.
  const FACE = 80;

  const cast = $derived(thePuppets());
  // Whoever is speaking now. A puppet is only ever listening or
  // speaking -- nothing gives it a turn to think about (see
  // `follow_state` in room.rs) -- so this is the actor with the floor.
  const speaking = $derived(cast.find((p) => p.state === 'speaking')?.name);
</script>

<!-- Nothing at all when nobody is on it. A stage is not a part of the
     room that is sometimes empty; it is there because there are
     characters standing on it, and it goes when the last of them is
     sent away. -->
{#if cast.length}
  <div class="stage" aria-label="On stage">
    <div class="boards">
      {#each cast as p (p.name)}
        <div class="actor" class:lit={p.name === speaking}>
          <!-- The same square as in the conversation, and the same
               button: clicking a character opens who they are, which
               is where the bio the AI wrote for them is. -->
          <Face who={p.name} agent size={FACE} />
          <span class="name">{p.name}</span>
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  /* Above the conversation and outside its scrollport, so the cast
     stays in front of you while the transcript of what they are saying
     scrolls past underneath. In the conversation's gutter, so the
     faces line up with the faces on the lines below them. */
  .stage {
    flex: 0 0 auto;
    padding: 10px var(--gutter) 12px;
    border-bottom: 1px solid var(--line);
    /* The floor: a shallow wash from the footlights up, which is the
       whole of the scenery. Enough to read as a place the characters
       are standing in rather than a row of pictures. */
    background:
      linear-gradient(
        to top,
        color-mix(in srgb, var(--agent) 9%, transparent),
        transparent 70%
      );
  }
  .boards {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: center;
    gap: 10px 18px;
  }
  .actor {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
    /* Room above for the lit one to rise into without the row moving:
       a stage where everybody shuffles down when somebody speaks is a
       stage nobody can follow. */
    padding-top: 4px;
    transition: transform 140ms ease-out;
  }
  .name {
    font-size: 12px;
    font-weight: 600;
    max-width: 10em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
  }

  /* Waiting their turn: still plainly there, and plainly not the one
     talking. Dimmed rather than greyed out -- a character standing in
     the half-dark is still in the scene. */
  .actor:not(.lit) { opacity: 0.55; }

  /* The one with the floor. Lit from below and lifted, which is what
     the eye picks up without having to be read. */
  .actor.lit { transform: translateY(-4px); }
  .actor.lit .name { color: var(--ink); }
  .actor.lit :global(.face) {
    border-color: var(--agent);
    box-shadow:
      0 0 0 2px color-mix(in srgb, var(--agent) 55%, transparent),
      0 6px 16px -4px color-mix(in srgb, var(--agent) 45%, transparent);
  }

  /* The lift is the only motion here, and it is the one thing worth
     turning off: the light alone says who is speaking. */
  @media (prefers-reduced-motion: reduce) {
    .actor { transition: none; }
    .actor.lit { transform: none; }
  }

  @media (prefers-color-scheme: dark) {
    .stage {
      background:
        linear-gradient(
          to top,
          color-mix(in srgb, var(--agent) 16%, transparent),
          transparent 70%
        );
    }
    /* Further down in the dark, where the lit one has more to be
       brighter than. */
    .actor:not(.lit) { opacity: 0.45; }
  }
</style>
