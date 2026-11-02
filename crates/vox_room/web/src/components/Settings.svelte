<script>
  import { room, setName, setPushToTalk, setVolume } from '../lib/room.svelte.js';
  import { NAME_MAX } from '../lib/names.js';

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

  // Who chose the name on your invite. The owner is reading their own
  // page when they read this one, and "the owner's to set" said to the
  // owner is a sentence about somebody who isn't there.
  const whoever = $derived(room.owner ? 'yours' : "whoever runs the room's");

  // The meter is linear amplitude; the ear is not. Same curve as the
  // strips in the room list, so the same loudness looks the same in
  // both places.
  const level = $derived(
    room.hearing > 0
      ? Math.max(0, Math.min(100, ((20 * Math.log10(room.hearing) + 60) / 60) * 100))
      : 0,
  );
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
    <p class="note">
      Everyone in this room and every other one sees this, on the roster and
      against everything you have said. Your microphone keeps running while it
      changes.
      {#if room.invited}
        You were invited as <strong>{room.invited}</strong>, which is
        {whoever}'s to set and stays as it is.
      {/if}
    </p>
  </section>

  <section>
    <h2>Your microphone</h2>
    <label class="check">
      <input type="checkbox" checked={room.pushToTalk}
        onchange={(e) => setPushToTalk(e.currentTarget.checked)} />
      <span>
        <strong>Push to talk</strong>
        <small>
          Your microphone stays muted except while you hold <kbd>Space</kbd> or the
          talk button. Everything said in one hold is one thing said, and the AI
          waits for you to let go however long you pause.
        </small>
      </span>
    </label>
    <p class="note">
      Mute, push to talk and the microphone belong to this <strong>tab</strong>, not
      to you: another tab of your own, or one left at the door, has no say in what
      the one you are speaking into does.
    </p>
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
      <span>Your volume</span>
      <input type="range" min="0" max="1.5" step="0.01" value={room.volume}
        oninput={(e) => setVolume(Number(e.currentTarget.value))} />
      <span class="much">{Math.round(room.volume * 100)}%</span>
    </label>
    <p class="slider">
      <span>Coming in</span>
      <span class="hear" title="What you hear: everyone but you"
        ><span style="width: {level}%"></span></span>
    </p>
    <p class="note">
      Yours alone, both of them: nobody else's hearing changes, and your own voice
      is never in your own mix. Turning one person down is on their fader, under
      the room.
    </p>
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
  .check { display: flex; align-items: flex-start; gap: 10px; }
  .check input { margin-top: 3px; }
  .check small, .note { display: block; color: var(--muted); font-size: 13px; line-height: 1.5; }
  .check small { margin-top: 2px; }
  .note { margin: 10px 0 0; }
  .back { text-align: center; }
  .manage { margin-top: 10px; }
  .slider { display: flex; align-items: center; gap: 10px; font-size: 13px; margin: 0 0 10px; }
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
  .hear { width: 120px; height: 6px; border-radius: 3px; background: var(--line); overflow: hidden; }
  .hear span { display: block; height: 100%; background: var(--meter); transition: width 80ms linear; }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; }
</style>
