<script>
  import { room, thePuppets, sitting, seatOf, takeSeat, stageLines } from '../lib/room.svelte.js';
  import Face from './Face.svelte';

  // The stage: the characters standing on it, what they are saying, and
  // whoever has come up to join them.
  //
  // It is here because a puppet show is the one thing in this room that
  // is *watched* rather than read. Everything else the room does is a
  // column of text and a row of faders, and that is right for a
  // conversation -- but a scene played by three characters is a scene,
  // and following it in a scrolling transcript means reading the name
  // on each line to work out who is talking. On the stage you look at
  // it: the one speaking is lit, and the others are waiting their turn.
  //
  // The lines are here and not in the conversation below, because a
  // play is not the room talking. What the conversation keeps is one
  // folded note at the curtain with the whole script in it.
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
  // People and bots who have come up to join the play, which is what
  // makes it an adlib.
  const up = $derived(sitting('stage'));
  const yours = $derived(seatOf(room.you) === 'stage');
  const lines = $derived(stageLines());
  const mode = $derived(room.show?.mode ?? 'scripted');
  const playing = $derived(room.show?.playing ?? null);

  // The stage log follows the last line, the way the conversation
  // follows the last thing said: a scene you have to scroll to keep up
  // with is a scene you are reading rather than watching.
  let box = $state();
  $effect(() => {
    lines.map((e) => e.text).join();
    if (box) queueMicrotask(() => (box.scrollTop = box.scrollHeight));
  });
</script>

<div class="stage" class:yours aria-label="The stage">
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
    {#each up as p (p.who)}
      <!-- Somebody who has come up to join the play. Drawn with the
           cast, because that is where they are, and apart from it,
           because they are not one of the characters. -->
      <div class="actor guest">
        <Face who={p.who} agent={p.bot} size={FACE} />
        <span class="name">{p.who}{#if p.who === room.you}<em> (you)</em>{/if}</span>
      </div>
    {/each}
  </div>

  {#if lines.length}
    <div class="script" bind:this={box} aria-label="What is being said on stage">
      {#each lines as e (e.id)}
        <p class:live={e.live}><strong>{e.who}</strong> {e.text}</p>
      {/each}
    </div>
  {/if}

  <!-- What kind of show this is, which is the one thing about it you
       cannot see by looking at the faces. -->
  <p class="how">
    <span class="mode" class:adlib={mode === 'adlib'}>{mode}</span>
    {#if mode === 'adlib'}
      the puppets are making it up with {up.map((p) => p.who).join(', ')}
    {:else if playing}
      “{playing.title}” — line {Math.min(playing.said + 1, playing.lines)} of {playing.lines}
    {:else}
      waiting between scenes
    {/if}
    {#if !yours}
      <button onclick={() => takeSeat('stage')}
        title="Go up and join the play. Any script stops, and the puppets start answering you.">
        Join the play
      </button>
    {/if}
  </p>
</div>

<style>
  /* Above the conversation and outside its scrollport, so the cast
     stays in front of you while the transcript of what the room has
     been saying scrolls past underneath. In the conversation's gutter,
     so the faces line up with the faces on the lines below them. */
  .stage {
    flex: 0 0 auto;
    padding: 10px var(--gutter) 8px;
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
  .stage.yours { box-shadow: inset 3px 0 0 var(--accent); }
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
  .name em { font-weight: 400; font-style: normal; }

  /* Waiting their turn: still plainly there, and plainly not the one
     talking. Dimmed rather than greyed out -- a character standing in
     the half-dark is still in the scene. */
  .actor:not(.lit) { opacity: 0.55; }
  /* Except somebody who has come up to join: they are not acting, so
     they are not waiting for a cue either. */
  .actor.guest { opacity: 1; }
  .actor.guest .name { color: var(--ink); }

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

  /* What is being said up here. Its own log, shallow on purpose: the
     scene is meant to be heard, and this is for catching the line you
     missed rather than for reading the play instead of watching it. */
  .script {
    margin: 8px 0 0;
    max-height: 7.5em;
    overflow-y: auto;
    overscroll-behavior: contain;
    font-size: 13px;
    line-height: 1.5;
  }
  .script p { margin: 0 0 3px; color: var(--muted); }
  .script strong { color: var(--agent); font-weight: 650; }
  /* Still being said: the words arrive as the audio plays. */
  .script p.live { color: var(--ink); }

  .how {
    display: flex;
    align-items: center;
    gap: 7px;
    margin: 7px 0 0;
    font-size: 11px;
    color: var(--muted);
  }
  .mode {
    flex: none;
    padding: 0 6px;
    border-radius: 4px;
    border: 1px solid var(--line);
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  /* Made up on the spot rather than read: worth telling apart at a
     glance, because it is the difference between a play you are
     watching and one you are in. */
  .mode.adlib {
    border-color: var(--accent);
    color: var(--accent);
  }
  .how button {
    margin-left: auto;
    flex: none;
    padding: 0 7px;
    font-size: 11px;
    line-height: 1.6;
    border-radius: 5px;
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
