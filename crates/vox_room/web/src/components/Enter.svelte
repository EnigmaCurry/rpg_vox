<script>
  import { onMount } from 'svelte';
  import {
    room, joinAudio, setName, logOut, setPushToTalk, loadDevices, watchDevices,
    setMicDevice, setSpeakerDevice, setLanguage, setConsent, theAI,
  } from '../lib/room.svelte.js';
  import { supported, canChooseSpeaker } from '../lib/audio.js';
  import { NAME_MAX } from '../lib/names.js';

  // The room is voice only, so this stands in front of it until you have
  // joined the audio: everyone in the room is in it by ear, not reading.
  let join = $state();
  $effect(() => join?.focus());

  // The name they want to be called, while they are typing it; until
  // they type, it is whatever the invite says, which arrives after this
  // card has been drawn. Only a link not yet spent can set one:
  // afterwards it is who they are to the room, to the conversation, and
  // to the cards they are holding.
  let typed = $state(null);
  const draft = $derived(typed ?? room.door?.display ?? '');

  async function save() {
    const ok = await setName(draft);
    // Saved, trimmed and all: show what the room took.
    if (ok) typed = null;
    return ok;
  }

  // Join Audio spends the link, so take a name typed but never blurred
  // with it; if the room won't have that name, nothing is spent.
  async function enter() {
    if (room.door && !(await save())) return;
    // The language shown at the door is only a suggestion until now --
    // from this browser, for somebody the room has never seen. Save it
    // on the way in, so the room mixes and translates by it.
    if (room.lang && room.lang !== mine) await setLanguage(room.lang);
    await joinAudio();
  }

  onMount(() => {
    // Asking for the mic now is what fills the device list with names,
    // and gets the permission prompt out of the way before the click
    // that matters.
    if (supported()) loadDevices({ permit: true });
    return watchDevices();
  });

  const starting = $derived(room.audio === 'starting');
  // What the room already has against your name, so the door only saves
  // a language when it is actually a change.
  const mine = $derived(
    room.people.find((p) => p.name === room.you)?.language || room.languages[0] || '',
  );
</script>

<main class="enter" aria-labelledby="enter-title">
  <div class="card">
    <!-- One name, said once. The eyebrow is there to say what the
         heading is when the heading is a game's name, and in a room
         with no game on it there is nothing for it to add. -->
    {#if room.game}<p class="eyebrow">Playing</p>{/if}
    <h1 id="enter-title">{room.game ?? 'Voice Room'}</h1>
    {#if room.door}
      <p class="hi">Hi, {room.door.name}!</p>
    {:else if room.you}
      <p class="hi">Hi, {room.you}!</p>
    {/if}
    <!-- Somebody else's link, picked up by this browser and never
         spent. It is said rather than acted on: a browser is one
         person, and being quietly handed another's name -- on a link
         clicked who knows when -- is how you end up in the room as
         them. Logging out below is what gives the room over to it. -->
    {#if room.holding}
      <p class="held">
        This browser is also holding <strong>{room.holding}</strong>'s invite link.
        You are in as {room.you}; log out below to come in as {room.holding} instead.
      </p>
    {/if}
    {#if !supported()}
      <p class="lead">
        This browser can't do the room's audio: it needs WebCodecs, which means a
        recent Chrome, Firefox or Safari.
      </p>
    {:else}
      <p class="lead">
        This is a voice and video conference room. Select your microphone and
        press <strong>Join Audio</strong> to enter.
        {#if room.door}
          Your link works once: pressing it is what spends it, and this
          browser is remembered from then on.
        {/if}
      </p>

      {#if room.door}
        <div class="devices name">
          <label>
            <span>Your name in the room</span>
            <input type="text" maxlength={NAME_MAX} value={draft}
              oninput={(e) => (typed = e.currentTarget.value)} onblur={save}
              onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); save(); } }} />
            <small>
              Everyone in the room sees this, and you can change it later. Your
              invite stays {room.door.name}'s, whatever you call yourself.
            </small>
          </label>
          {#if room.nameError}<p class="warn">{room.nameError}</p>{/if}
        </div>
      {/if}
      {#if room.doorError}
        <p class="warn door">{room.doorError}</p>
      {/if}

      <div class="devices">
        <label>
          <span>Microphone</span>
          <select value={room.micId} onchange={(e) => setMicDevice(e.currentTarget.value)}>
            <option value="">System default</option>
            {#each room.devices.mics as d (d.id)}
              <option value={d.id}>{d.label}</option>
            {/each}
          </select>
        </label>
        {#if canChooseSpeaker()}
          <label>
            <span>Speakers</span>
            <select value={room.speakerId} onchange={(e) => setSpeakerDevice(e.currentTarget.value)}>
              <option value="">System default</option>
              {#each room.devices.speakers as d (d.id)}
                <option value={d.id}>{d.label}</option>
              {/each}
            </select>
          </label>
        {/if}
        {#if room.languages.length > 1}
          <label>
            <span>Language</span>
            <!-- Every option is its own name, the room's own included.
                 An empty value for the first one meant the box showed
                 nothing at all to anybody the room already knew about,
                 since what the room says back is the name. -->
            <select value={room.lang} onchange={(e) => setLanguage(e.currentTarget.value)}>
              {#each room.languages as l (l)}
                <option value={l}>{l}</option>
              {/each}
            </select>
          </label>
        {/if}
        {#if room.langError}
          <p class="warn">Your language: {room.langError}</p>
        {/if}
        {#if room.devicesError}
          <p class="warn">
            The browser wouldn't give us the microphone ({room.devicesError}), so your
            devices can't be listed by name. Allow it and reload to choose.
          </p>
        {/if}
      </div>

      <!-- What coming in actually does to you, before you do it. The
           tick is kept in this browser, so it is asked once; unticking
           it takes it back, and shuts the door again. -->
      <div class="consent" class:agreed={room.consented}>
        <p>
          Your microphone goes live to everyone else here. What you say is
          transcribed on this server and written into the conversation —
          which everyone here reads, and which the room keeps until somebody
          deletes it. That writing is sent on to the AI, and to the
          translation and voice services this room is set up to use, which
          are run by whoever runs it.
        </p>
        <label>
          <input type="checkbox" checked={room.consented}
            onchange={(e) => setConsent(e.currentTarget.checked)} />
          <span>I agree to be recorded and transcribed.</span>
        </label>
      </div>

      <button class="primary join" bind:this={join} onclick={enter}
        disabled={starting || !room.consented}
        title={room.consented ? undefined : 'Agree above to come in.'}>
        {starting ? 'Starting…' : 'Join Audio'}
      </button>
      {#if room.audioError}
        <p class="warn">Your microphone: {room.audioError}</p>
      {/if}

      <label class="ptt">
        <input type="checkbox" checked={room.pushToTalk}
          onchange={(e) => setPushToTalk(e.currentTarget.checked)} />
        <span>
          Push to talk
          <small>
            On speakers, turn this on: your mic opens only while you hold Space.
            With a headset, leave it off — anyone can interrupt {theAI()}
            just by talking.
          </small>
        </span>
      </label>
    {/if}

    <!-- Nothing of the room is said here, and nothing of it is open:
         the sockets are the room's, and at the door you are not in it.
         Who is in, what is being said, which rooms there are -- all of
         it arrives when you come in, and goes when you leave. -->

    <!-- The way out of a browser that is signed in. An invite not yet
         spent has nothing to give back. -->
    {#if !room.door}
      <p class="out">
        <button class="logout" onclick={logOut}
          title="Forget this browser. Your invite link is spent, so coming back takes a new one.">
          Log out
        </button>
      </p>
    {/if}
  </div>
</main>

<style>
  /* A page, not a sheet over one. Until the audio is joined there is
     nothing behind this to look at: the room is voice, and a
     conversation you can read but not hear is not being in it. */
  .enter {
    height: 100%;
    display: grid;
    place-items: center;
    padding: 24px;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .card {
    width: min(26em, 100%);
    text-align: center;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 16px;
    box-shadow: var(--shadow), 0 12px 40px rgb(0 0 0 / 12%);
    padding: 28px 28px 22px;
  }
  .eyebrow {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--muted);
    margin: 0 0 2px;
  }
  h1 { font-size: 24px; margin: 0 0 10px; }
  .hi { font-size: 15px; font-weight: 600; margin: 0 0 8px; }
  .lead { color: var(--muted); margin: 0 0 18px; }
  .lead strong { color: var(--ink); font-weight: 600; white-space: nowrap; }
  /* What coming in does to you. Plain and above the button, because it
     is the thing the button is asking about. */
  .consent {
    text-align: left;
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 10px 12px;
    margin-bottom: 14px;
    background: var(--bg);
  }
  .consent.agreed { border-color: color-mix(in srgb, var(--ok) 45%, var(--line)); }
  .consent p { font-size: 12px; line-height: 1.5; color: var(--muted); margin: 0 0 8px; }
  .consent label { display: flex; align-items: flex-start; gap: 8px; font-size: 13px; }
  .consent input { margin: 2px 0 0; flex: none; }
  .devices { display: flex; flex-direction: column; gap: 10px; margin-bottom: 18px; text-align: left; }
  .devices label { display: flex; flex-direction: column; gap: 4px; }
  .devices span { font-size: 12px; color: var(--muted); }
  select {
    font: inherit;
    width: 100%;
    color: var(--ink);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 8px 10px;
  }
  select:hover { border-color: var(--muted); }
  .devices.name { margin-bottom: 14px; }
  .devices.name small { font-size: 12px; color: var(--muted); line-height: 1.45; }
  input[type='text'] {
    font: inherit;
    width: 100%;
    color: var(--ink);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 8px 10px;
  }
  input[type='text']:hover { border-color: var(--muted); }
  .warn.door { margin: 0 0 14px; }
  /* A link this browser is carrying for somebody else: worth reading,
     not a thing that has gone wrong. */
  .held {
    text-align: left;
    font-size: 13px;
    line-height: 1.45;
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 8px 12px;
    margin: 0 0 14px;
  }
  .held strong { color: var(--ink); }
  .join {
    width: 100%;
    font-size: 17px;
    font-weight: 600;
    padding: 14px 20px;
    border-radius: 10px;
  }
  .join:not(:disabled):hover { filter: brightness(1.08); }
  .ptt {
    display: flex;
    gap: 10px;
    text-align: left;
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--line);
    cursor: pointer;
  }
  .ptt input { margin-top: 3px; }
  .ptt small { display: block; font-size: 12px; color: var(--muted); line-height: 1.45; }
  .warn { color: var(--danger); font-size: 13px; margin: 12px 0 0; text-align: left; }
  .devices .warn { margin: 2px 0 0; }
  .out { margin: 14px 0 0; padding-top: 12px; border-top: 1px solid var(--line); }
  .logout {
    font-size: 13px;
    color: var(--muted);
    background: none;
    border: 0;
    padding: 4px 8px;
  }
  .logout:hover { color: var(--danger); }
</style>
