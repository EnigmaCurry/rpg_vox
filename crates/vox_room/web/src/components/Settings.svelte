<script>
  import { room, setName, setVolume, avatarOf, forgetAvatar } from '../lib/room.svelte.js';
  import { NAME_MAX } from '../lib/names.js';
  import Face from './Face.svelte';

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
          {room.agents[0]?.name ?? 'the AI'} out loud to make you one.
        </p>
      {/if}
    </div>
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

  <section>
    <h2>What you hear</h2>
    <label class="slider">
      <span>App volume</span>
      <input type="range" min="0" max="1.5" step="0.01" value={room.volume}
        oninput={(e) => setVolume(Number(e.currentTarget.value))} />
      <span class="much">{Math.round(room.volume * 100)}%</span>
    </label>
  </section>

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
  header { display: flex; align-items: center; gap: 12px; margin-bottom: 8px; }
  .spacer { flex: 1; }
  h1 { font-size: 20px; margin: 0; }
  h2 { font-size: 14px; margin: 0 0 10px; color: var(--muted); font-weight: 600; }
  section {
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--panel);
    padding: 14px 16px;
    margin-top: 16px;
  }
  .note { display: block; color: var(--muted); font-size: 13px; line-height: 1.5; margin: 10px 0 0; }
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
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 8px 10px;
  }
  .name input:hover { border-color: var(--muted); }
  .warn { color: var(--danger); font-size: 13px; margin: 8px 0 0; }
  .much { color: var(--muted); min-width: 3.5ch; text-align: right; }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; }
  .face-row { display: flex; align-items: flex-start; gap: 12px; }
  /* The square is drawn to sit beside a name in the conversation, and
     there is no name above it here. */
  .face-row :global(.face) { margin-top: 0; }
  .face-row .hint { margin: 0 0 8px; color: var(--muted); }
</style>
