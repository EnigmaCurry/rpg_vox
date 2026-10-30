<script>
  import { onMount } from 'svelte';
  import {
    room, joinAudio, setName, logOut, setPushToTalk, loadDevices, watchDevices,
    setMicDevice, setSpeakerDevice, setLanguage,
  } from '../lib/room.svelte.js';
  import { supported, canChooseSpeaker } from '../lib/audio.js';
  import { hue } from '../lib/color.js';
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

  const here = $derived(
    room.people.filter((p) => p.online && p.name !== room.you).map((p) => p.name),
  );
  const starting = $derived(room.audio === 'starting');
  // What the room already has against your name, so the door only saves
  // a language when it is actually a change.
  const mine = $derived(
    room.people.find((p) => p.name === room.you)?.language ?? '',
  );
</script>

<div class="enter" role="dialog" aria-modal="true" aria-labelledby="enter-title">
  <div class="card">
    <p class="eyebrow">{room.game ? 'Playing' : 'Voice room'}</p>
    <h1 id="enter-title">{room.game ?? 'Voice room'}</h1>
    {#if room.door}
      <p class="hi">Hi, {room.door.name}!</p>
    {:else if room.you}
      <p class="hi">Hi, {room.you}!</p>
    {/if}
    {#if !supported()}
      <p class="lead">
        This browser can't do the room's audio: it needs WebCodecs, which means a
        recent Chrome, Firefox or Safari.
      </p>
    {:else}
      <p class="lead">
        You are entering a voice conference room. Select your device and click
        <strong>Join Audio</strong> to enter the room.
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
              Everyone in the room sees this. Your invite stays {room.door.name}'s,
              whatever you call yourself.
            </small>
          </label>
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
            <select value={room.lang} onchange={(e) => setLanguage(e.currentTarget.value)}>
              {#each room.languages as l, i (l)}
                <option value={i === 0 ? '' : l}>{l}</option>
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

      <button class="primary join" bind:this={join} onclick={enter} disabled={starting}>
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
            With a headset, leave it off — anyone can interrupt {room.agent.name}
            just by talking.
          </small>
        </span>
      </label>
    {/if}

    <!-- Who is in is the room telling you, live. A link not yet spent has
         no socket to hear it on, so the card says nothing rather than
         something that was true once. -->
    {#if !room.door}
    <p class="who">
      {#if !room.connected}
        Connecting to the room…
      {:else if here.length === 0}
        Nobody else is here yet.
      {:else}
        <span class="dots" aria-hidden="true">
          {#each here as name (name)}
            <span class="pip" style="--h: {hue(name)}" title={name}></span>
          {/each}
        </span>
        {here.join(', ')}
        {here.length === 1 ? 'is' : 'are'} already in.
      {/if}
    </p>
    {/if}

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
</div>

<style>
  .enter {
    position: fixed;
    inset: 0;
    z-index: 30;
    display: grid;
    place-items: center;
    padding: 24px;
    overflow-y: auto;
    background: color-mix(in srgb, var(--bg) 82%, transparent);
    backdrop-filter: blur(6px);
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
  .who {
    font-size: 13px;
    color: var(--muted);
    margin: 18px 0 0;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .out { margin: 14px 0 0; padding-top: 12px; border-top: 1px solid var(--line); }
  .logout {
    font-size: 13px;
    color: var(--muted);
    background: none;
    border: 0;
    padding: 4px 8px;
  }
  .logout:hover { color: var(--danger); }
  .dots { display: inline-flex; gap: 3px; }
  .pip { width: 8px; height: 8px; border-radius: 50%; background: hsl(var(--h) 55% 55%); }
</style>
