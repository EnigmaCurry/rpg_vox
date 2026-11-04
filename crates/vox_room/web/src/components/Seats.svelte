<script>
  import { room, sitting, seatOf, takeSeat } from '../lib/room.svelte.js';
  import Face from './Face.svelte';

  // One part of the playhouse, with whoever is in it and a way in.
  //
  // The stage draws its own (it has the puppets, the log and the
  // scripted/adlib line); this is the plain one, used for the
  // director's box and the audience.
  //
  // `seat` is which part, `title` what it is called, `why` the line
  // under the title saying what being in it means. `size` is how big
  // the faces are: smaller than the stage's, because the people in the
  // box and the audience are not the thing being watched.
  //
  // `lines` is what has been said in this part, for a part that keeps
  // its own log. The box does, because the conversation below is the
  // audience's and the two must not be mingled; the audience's log *is*
  // that conversation, so it passes none.
  let { seat, title, why, size = 40, lines = [] } = $props();

  const here = $derived(sitting(seat));
  const yours = $derived(seatOf(room.you) === seat);
  // The AI running the show is in the box for as long as it lasts and
  // cannot be moved, so the room does not offer to move it.
  const director = $derived(room.seats?.director);

  // Read in your own language where there is one for you, as a line in
  // the conversation is: a room that moved the box's talk up here and
  // left somebody reading in another tongue with the original would
  // have taken their interpreter away to tidy the screen. The original
  // is what there is until the translation lands, and what there is at
  // all in a room with one language.
  const mine = $derived(room.lang || room.languages[0] || '');
  const words = (e) => e.translations?.[mine]?.trim() || e.text;

  // Follows the last thing said, the way the stage's log and the
  // conversation both do: a few lines deep, so the band stays a band.
  let tail = $state();
  $effect(() => {
    lines.map((e) => e.text).join();
    if (tail) queueMicrotask(() => (tail.scrollTop = tail.scrollHeight));
  });
</script>

<div class="part" class:yours>
  <div class="who">
    <h3>
      {title}
      {#if !yours}
        <!-- The one control a part has. Moving yourself is what this
             is for; the seat that cannot be moved says so itself when
             anybody asks, rather than being hidden here. -->
        <button onclick={() => takeSeat(seat)} title={why}>Sit here</button>
      {/if}
    </h3>
    {#if here.length}
      <div class="faces">
        {#each here as p (p.who)}
          <span class="sat" class:bot={p.bot} class:runs={p.who === director}>
            <Face who={p.who} agent={p.bot} size={size} />
            <span class="name">{p.who}{#if p.who === room.you}<em> (you)</em>{/if}</span>
          </span>
        {/each}
      </div>
    {:else}
      <p class="nobody">{why}</p>
    {/if}
    {#if lines.length}
      <!-- What has been said in here, which for the box is the whole
           point of it: this is the conversation the director is
           actually having, and the one the audience's transcript has
           no part of. Shallow on purpose, like the stage's. -->
      <div class="said-here" bind:this={tail} aria-label="What is being said in the {title}">
        {#each lines as e (e.id)}
          <p class:live={e.live} class:bot={e.kind === 'agent'}>
            <strong>{e.who}</strong> {words(e)}
          </p>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .part {
    padding: 8px var(--gutter) 9px;
    border-bottom: 1px solid var(--line);
  }
  /* The part you are in. A faint edge down the side, because on a
     screen with three of these the first thing you want to know is
     which one is yours. */
  .part.yours { box-shadow: inset 3px 0 0 var(--accent); }
  h3 {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin: 0 0 6px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  h3 button {
    margin-left: auto;
    padding: 0 7px;
    font-size: 11px;
    line-height: 1.6;
    border-radius: 5px;
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
  }
  .faces { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 14px; }
  .sat { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .name {
    font-size: 12px;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 10em;
  }
  .name em { font-weight: 400; color: var(--muted); font-style: normal; }
  /* Whose show it is. Said in the colour the room already uses for its
     AIs, so the box reads as theirs at a glance. */
  .sat.runs .name { color: var(--agent); }
  .nobody { margin: 0; font-size: 12px; color: var(--muted); }

  /* What is being said in this part. Set the same way as the stage's
     log and for the same reason: it is for catching the line you
     missed, not for reading instead of listening. */
  .said-here {
    margin: 7px 0 0;
    max-height: 7.5em;
    overflow-y: auto;
    overscroll-behavior: contain;
    font-size: 13px;
    line-height: 1.5;
  }
  .said-here p { margin: 0 0 3px; color: var(--muted); }
  .said-here strong { color: var(--ink); font-weight: 650; }
  /* An AI's words, in the colour the room already uses for them. */
  .said-here p.bot strong { color: var(--agent); }
  /* Still being said: the words arrive as the audio plays. */
  .said-here p.live { color: var(--ink); }
</style>
