<script>
  import {
    room, setName, setVolume, avatarOf, forgetAvatar, myFaces, faceAt, wearFace,
    command, setPttHush, setBotsOff, theAI, theAIs,
  } from '../lib/room.svelte.js';
  import { theme, setTheme, THEMES } from '../lib/theme.svelte.js';
  import { NAME_MAX } from '../lib/names.js';
  import { scaledDown } from '../lib/pixels.js';
  import Face from './Face.svelte';

  // How big a square in the row of faces you have worn is, in one
  // place: the CSS draws it and `lib/pixels.js` is told it, and the
  // two must agree.
  const PAST = 56;

  // The things you set once, off the floor of the room.
  //
  // The footer is for what you reach for while talking -- mute, your
  // desktop, the way out -- and everything else that used to be strung
  // along it is here. A page rather than a sheet over the room: these
  // are not decisions you make while watching the conversation, and the
  // room is still running behind it. The audio never stops, and the
  // space bar still works, so you can hear what a change did.
  function onkeydown(e) {
    if (e.key === 'Escape') room.settings = false;
  }

  // The name you are typing, until the room has it; `null` means the
  // box is showing what the room calls you, which is what it shows
  // whenever you are not in the middle of changing it.
  let typed = $state(null);
  const draft = $derived(typed ?? room.you ?? '');

  // The AIs in the room whose voice is theirs to settle. Not the
  // puppets: a puppet's voice is the part it plays, decided when the
  // character was made up, and there is nothing here to choose.
  const speakers = $derived(room.agents.filter((b) => !b.puppet));

  // A room can have more than one AI in it, and prose written for one
  // of them reads wrong the moment it does.
  const one = $derived(theAIs().length === 1);

  // The faces you have worn, newest first. Re-read whenever the one
  // you are wearing changes -- the AI may have given you another
  // while this page is open, and that one belongs in the row too.
  let faces = $state([]);
  $effect(() => {
    room.avatars[room.you];
    myFaces().then((f) => (faces = f));
  });

  // Which of those squares is scaling its picture *down* rather than
  // up, which decides how each is drawn -- see `lib/pixels.js`. By
  // version and not one flag, because the ten faces in this row are
  // not all the same size: a 64-pixel sprite somebody dropped in is
  // being blown up in a 56-pixel square, and anything a picture
  // server made -- a cel-shaded portrait at 768, or a sprite handed
  // back magnified so it reads in a chat -- is being shrunk by ten
  // times or more, which nearest neighbour turns to noise.
  let smooth = $state({});
  const sized = (at, img) => {
    if (scaledDown(img, PAST)) smooth = { ...smooth, [at]: true };
  };

  async function save() {
    // Nothing typed, or typed back to what it already was: there is no
    // rename to ask for, and no error to clear off the page.
    if (typed === null) return;
    if (await setName(draft)) typed = null;
  }

</script>

<svelte:window {onkeydown} />

<main class="settings">
  <header>
    <h1>Settings</h1>
    <div class="spacer"></div>
    <button class="primary" onclick={() => (room.settings = false)}>Done <kbd>Esc</kbd></button>
  </header>

  <!-- The one name here that is yours. Whoever runs the room picked
       the name on your invite -- it is what you are revoked by and what
       your own room is yours by, so it never moves -- and this is the
       one everybody actually reads. Changing it changes it everywhere,
       the conversation included: the room calls you one thing, not a
       different thing per paragraph. -->
  <section>
    <h2>Your name</h2>
    <label class="name">
      <span>What the room calls you</span>
      <input type="text" maxlength={NAME_MAX} value={draft}
        oninput={(e) => (typed = e.currentTarget.value)} onblur={save}
        onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); e.currentTarget.blur(); } }} />
    </label>
    {#if room.nameError}<p class="warn">{room.nameError}</p>{/if}
  </section>

  <!-- Yours to be rid of, and nobody else's: the server refuses any
       name but your own, whoever asks. Making one is a thing you ask
       the AI for out loud -- there is nothing to press here for that,
       because it is an interview and not a form. -->
  <section>
    <h2>Your face</h2>
    <div class="face-row">
      <Face who={room.you} opens={false} />
      {#if avatarOf(room.you)}
        <div>
          <p class="hint">The square beside everything you say.</p>
          <button onclick={forgetAvatar}>Take it off</button>
        </div>
      {:else}
        <p class="hint">
          You have none, so your lines carry your initials. Ask
          {room.agents[0]?.name ?? 'the AI'} out loud to make you one{#if faces.length},
          or put one of these back on{/if}.
        </p>
      {/if}
    </div>

    <!-- The ones you have worn before. Choosing again from what has
         already been made costs nothing and needs no AI, so it is a
         button rather than something to ask for out loud. -->
    {#if faces.length > 1 || (faces.length === 1 && !faces[0].worn)}
      <div class="past">
        {#each faces as f (f.at)}
          <button
            class="past-face"
            class:worn={f.worn}
            title={f.prompt || 'A face you have worn'}
            aria-pressed={f.worn}
            onclick={() => !f.worn && wearFace(f.at)}
          >
            <img
              src={faceAt(f.at)}
              alt={f.prompt}
              class:smooth={smooth[f.at]}
              onload={(e) => sized(f.at, e.currentTarget)}
            />
          </button>
        {/each}
      </div>
      <p class="hint">
        The last {faces.length} you have worn, newest first. The one you are
        wearing is outlined; press another to put it back on.
      </p>
    {/if}
  </section>

  <section>
    <h2>Your microphone</h2>
    {#if room.micName}
      <p class="note">
        The microphone the browser gave the room: <strong>{room.micName}</strong>.
        Pick a different one at the door, before you come in.
      </p>
    {:else}
      <p class="note">The browser hasn't named the microphone it gave the room.</p>
    {/if}
  </section>

  <!-- What push to talk *means*, which is not the same question as
       whether it is on. The switch for that is beside the button it
       changes, down in the footer, because it is a thing you flip while
       talking; this is a thing you settle once, when you decide where
       you are sitting and what you are listening on. Both ways are
       right for somebody: see `setPttHush`. -->
  <section>
    <h2>Holding the talk button</h2>
    <p class="note first">
      With push to talk off, your mic is simply open and everybody hears
      everybody. With it on, this is what holding it does to the room's own
      sound.
    </p>
    <label class="duplex">
      <input type="radio" name="duplex" checked={!room.pttHush}
        onchange={() => setPttHush(false)} />
      <span>
        You keep hearing the room
        <small>
          Full duplex. You hear everybody while you talk, so holding the key
          to interrupt lets you hear the AI stop. Right for a headset or
          headphones.
        </small>
      </span>
    </label>
    <label class="duplex">
      <input type="radio" name="duplex" checked={room.pttHush}
        onchange={() => setPttHush(true)} />
      <span>
        The room goes quiet
        <small>
          Half duplex. Everything the room is saying is cut for as long as you
          hold, and comes back once your mic has closed. Right for speakers:
          what they play is in front of your microphone, and it would be picked
          up, written down as something you said, and read back to the room as
          yours.
        </small>
      </span>
    </label>
  </section>

  <!-- The other half of the footer's pair: what BOTS off means. The
       switch itself is down there, because turning the AIs off is
       something you do in the middle of a conversation; which of the
       two ways it is off is a thing about you and your table, and it
       is settled here. See `setBotsOff`. -->
  <section>
    <h2>Turning {one ? 'the AI' : 'the AIs'} off</h2>
    <p class="note first">
      <strong>BOTS</strong> in the footer says whether {theAI('or')}
      {one ? 'may answer' : 'may answer'} what you say. It is yours alone — turning it
      off changes nothing for anybody else in the room, and everything said is
      still written into the one conversation. This is what off means for you.
    </p>
    <label class="duplex">
      <input type="radio" name="bots-off" checked={room.botsOff === 'named'}
        onchange={() => setBotsOff('named')} />
      <span>
        Unless I say its name
        <small>
          {one ? 'It stays' : 'They stay'} out of what you say, and still
          {one ? 'answers' : 'answer'} when you call {one ? 'it' : 'one of them'} by
          name — so a quiet room is still a room you can ask a question in.
        </small>
      </span>
    </label>
    <label class="duplex">
      <input type="radio" name="bots-off" checked={room.botsOff === 'never'}
        onchange={() => setBotsOff('never')} />
      <span>
        Never
        <small>
          Nothing you say reaches {one ? 'it' : 'them'}, its own name included.
          Someone else in the room can still bring {one ? 'it' : 'them'} in, and
          you will hear the answer.
        </small>
      </span>
    </label>
  </section>

  <section>
    <h2>What you hear</h2>
    <label class="slider">
      <span>App volume</span>
      <input type="range" min="0" max="1.5" step="0.01" value={room.volume}
        oninput={(e) => setVolume(Number(e.currentTarget.value))} />
      <span class="much">{Math.round(room.volume * 100)}%</span>
    </label>
  </section>

  <!-- What the room looks like, which is this browser's business and
       nobody else's: it is not sent anywhere, and two people in the
       same room can be sitting in two different ones without either
       being able to tell. Kept per browser for the same reason -- the
       same person at a desk and on a phone may well want different
       answers. See `lib/theme.svelte.js`. -->
  <section>
    <h2>The look</h2>
    <p class="note first">
      Yours alone, and only on this browser. Nobody else in the room sees it
      change.
    </p>
    {#each THEMES as t (t.id)}
      <label class="duplex">
        <input type="radio" name="theme" checked={theme.id === t.id}
          onchange={() => setTheme(t.id)} />
        <span>
          {t.name}
          <small>{t.blurb}</small>
        </span>
      </label>
    {/each}
  </section>

  {#if room.voices.length}
    <section>
      <h2>Voices</h2>
      <p class="note">
        Voices the AI has cloned from somebody in the house. Choosing one here
        settles which voice it speaks in — in every room, not just this one, and
        after a restart. It is the same AI down the hall. Asking it out loud to
        speak in another is still the quicker way, and that one lasts the
        afternoon and only in the room you asked in; this is what it goes back
        to.
      </p>
      {#each speakers as bot (bot.name)}
        <label class="voice">
          <span class="what">{bot.name} speaks in</span>
          <select value={bot.voice ?? ''}
            aria-label="The voice {bot.name} speaks in"
            onchange={(e) =>
              command({ type: 'voice', bot: bot.name, voice: e.currentTarget.value || null })}>
            <option value="">Its own voice</option>
            {#each room.voices as v (v.id)}
              <option value={v.name}>{v.name} — {v.who}'s voice</option>
            {/each}
          </select>
        </label>
      {/each}
      <p class="note">
        Each one is somebody in the house, read aloud by them. Ask
        {room.agents[0]?.name ?? 'the AI'} out loud to clone another or to delete
        one — and it is yours to ask about if one of them is your voice.
      </p>
    </section>
  {/if}

  {#if room.owner}
    <section>
      <h2>The house</h2>
      <p class="note">
        You own this room: you make the rooms, invite people, say who may go where, and
        can take any of it back. Nobody else sees this.
      </p>
      <button class="primary manage" onclick={() => { room.settings = false; room.managing = true; }}>
        People and rooms
      </button>
    </section>
  {/if}

  <p class="note back">
    The room is still running: you are still in the audio, and <kbd>Space</kbd>
    still mutes or talks from here.
  </p>
</main>

<style>
  .settings {
    max-width: 44em;
    margin: 0 auto;
    padding: 24px 16px 48px;
    height: 100%;
    overflow: auto;
    overscroll-behavior: contain;
  }
  .voice { display: flex; align-items: center; gap: 10px; margin: 0 0 10px; }
  .voice .what { font-size: 14px; }
  .voice select { flex: 1; min-width: 0; }
  header { display: flex; align-items: center; gap: 12px; margin-bottom: 8px; }
  .spacer { flex: 1; }
  h1 { font-size: 20px; margin: 0; }
  h2 { font-size: 14px; margin: 0 0 10px; color: var(--muted); font-weight: 600; }
  section {
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card-image) var(--panel);
    padding: 14px 16px;
    margin-top: 16px;
  }
  .note { display: block; color: var(--muted); font-size: 13px; line-height: 1.5; margin: 10px 0 0; }
  /* The one that comes before what it is about, rather than after it:
     the heading has already said what this is, and a gap under the
     heading as well as over the first choice reads as a missing line. */
  .note.first { margin: 0 0 12px; }
  .duplex { display: flex; align-items: flex-start; gap: 10px; font-size: 14px; cursor: pointer; margin: 0 0 10px; }
  .duplex:last-child { margin-bottom: 0; }
  .duplex input { margin-top: 3px; }
  .duplex small { display: block; font-size: 12px; color: var(--muted); line-height: 1.45; margin-top: 2px; }
  .back { text-align: center; }
  .manage { margin-top: 10px; }
  .slider { display: flex; align-items: center; gap: 10px; font-size: 13px; margin: 0; }
  .slider input[type='range'] { flex: 1; max-width: 18em; }
  .name { display: flex; flex-direction: column; gap: 4px; max-width: 22em; }
  .name span { font-size: 12px; color: var(--muted); }
  .name input {
    font: inherit;
    width: 100%;
    color: var(--ink);
    background: var(--field-bg);
    border: 1px solid var(--field-border);
    border-radius: var(--field-radius);
    padding: 8px 10px;
  }
  .name input:hover { border-color: var(--muted); }
  .warn { color: var(--danger); font-size: 13px; margin: 8px 0 0; }
  .much { color: var(--muted); min-width: 3.5ch; text-align: right; }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; }
  .face-row { display: flex; align-items: flex-start; gap: 12px; }
  .face-row .hint { margin: 0 0 8px; color: var(--muted); }
  .past { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 12px; }
  .past-face {
    padding: 0;
    /* `PAST` above is this number; they are read together. */
    width: 56px;
    height: 56px;
    border-radius: 8px;
    overflow: hidden;
    border: 1px solid var(--line);
    background: var(--bubble);
    box-shadow: none;
    cursor: pointer;
  }
  /* The one being worn is not a thing to press, so it does not look
     like one -- it is the marker for where you are in the row. */
  .past-face.worn { border: 2px solid var(--agent); cursor: default; }
  .past-face:not(.worn):hover { border-color: var(--agent); }
  .past-face img {
    width: 100%;
    height: 100%;
    /* A sprite blown up into the square: nearest neighbour, which is
       the whole point of the form. */
    image-rendering: pixelated;
    object-fit: cover;
    display: block;
  }
  /* And one bigger than the square, which this is shrinking: a
     full-resolution picture, or a photograph somebody dropped in.
     Nearest neighbour throws away thirteen pixels in fourteen on the
     way down, which is noise and not a face. */
  .past-face img.smooth { image-rendering: auto; }
</style>
