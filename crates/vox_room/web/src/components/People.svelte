<script>
  import {
    room,
    ask,
    loadPeople,
    invitePerson,
    relinkPerson,
    uninvitePerson,
    setPersonRoom,
    setBotRoom,
    makeRoom,
    renameRoom,
    deleteRoom,
  } from '../lib/room.svelte.js';

  // The house, for the one who owns it: the rooms it has, who is
  // invited, and which of those rooms each of them may go to.
  //
  // A page rather than a panel for the same reason the settings are
  // one: nothing here is watched while it is used, and an invite link
  // wants room to be read and copied. Nothing on it is what keeps
  // anybody out either -- every one of these asks the server, and the
  // server asks who is asking.
  let house = $state({
    loading: true,
    error: null,
    people: [],
    bots: [],
    allRooms: [],
    lobby: null,
    own: '@',
    you: '',
  });
  let name = $state('');
  let busy = $state(null);
  // The last link made or asked for, to be copied out of. Shown until
  // the owner is done with it rather than flashing past.
  let fresh = $state(null);
  let copied = $state(null);

  async function load() {
    const d = await loadPeople();
    if (!d) {
      house.loading = false;
      house.error = 'The room wouldn’t say. Only the owner can see this.';
      return;
    }
    house = { loading: false, error: null, ...d };
  }
  $effect(() => {
    load();
  });

  const full = (link) => new URL(link, location.origin).toString();

  function onkeydown(e) {
    if (e.key === 'Escape') room.managing = false;
  }

  async function copy(link) {
    const text = full(link);
    try {
      await navigator.clipboard.writeText(text);
      copied = text;
      setTimeout(() => (copied === text ? (copied = null) : null), 2000);
    } catch {
      // No clipboard (an insecure origin, or a browser that won't):
      // the link is on screen to be selected, which is what it is for.
      copied = null;
    }
  }

  async function doing(what, fn) {
    busy = what;
    house.error = null;
    try {
      return await fn();
    } catch (e) {
      house.error = e.message || String(e);
      return null;
    } finally {
      busy = null;
      await load();
    }
  }

  const invite = () =>
    doing('invite', async () => {
      const d = await invitePerson(name);
      fresh = { name: d.name, link: d.link, why: 'invited' };
      name = '';
    });

  const relink = (who) =>
    doing(`link:${who}`, async () => {
      const d = await relinkPerson(who);
      fresh = { name: d.name, link: d.link, why: 'a new link' };
    });

  // Removing somebody cuts the browsers they are sitting in, so it
  // asks first -- with the room's own dialog, which is the one every
  // other question in here uses.
  async function remove(who) {
    const sure = await ask(
      `Remove ${who}? Their link stops working, the browsers they are in are signed out, ` +
        `and the rooms they were given go with them.`,
      'Remove',
    );
    if (!sure) return;
    await doing(`drop:${who}`, async () => {
      await uninvitePerson(who);
      if (fresh?.name === who) fresh = null;
    });
  }

  const toggle = (who, id, allowed) =>
    doing(`room:${who}:${id}`, () => setPersonRoom(who, id, allowed));

  const toggleBot = (who, id, allowed) =>
    doing(`bot:${who}:${id}`, () => setBotRoom(who, id, allowed));

  // -- the rooms themselves --------------------------------------------

  let newRoom = $state('');
  // Which room is being renamed, and to what. One at a time: the box
  // you are typing in is the room you are renaming.
  let naming = $state(null);

  const make = () =>
    doing('make', async () => {
      await makeRoom(newRoom);
      newRoom = '';
    });

  function startNaming(r) {
    naming = { id: r.id, title: r.title };
  }

  const saveName = () =>
    doing('rename', async () => {
      const { id, title } = naming;
      naming = null;
      await renameRoom(id, title);
    });

  async function dropRoom(r) {
    const sure = await ask(
      `Take ${r.title} out of the house? Anybody in it goes back to the lobby, and nobody ` +
        `can go to it again. What was said in it is left in the database, out of sight.`,
      'Take it away',
    );
    if (!sure) return;
    await doing(`drop-room:${r.id}`, () => deleteRoom(r.id));
  }

  /// The rooms that are given out: every shared room but the lobby,
  /// which everybody has.
  const given = $derived(house.allRooms.filter((r) => r.id !== house.lobby));
</script>

<svelte:window {onkeydown} />

<main class="people">
  <header>
    <h1>People and rooms</h1>
    <div class="spacer"></div>
    <button class="primary" onclick={() => (room.managing = false)}>Done <kbd>Esc</kbd></button>
  </header>

  {#if house.loading}
    <p class="note">Asking the room…</p>
  {:else if house.error && !house.people.length}
    <p class="warn">{house.error}</p>
  {:else}
    {#if house.error}<p class="warn">{house.error}</p>{/if}

    <section>
      <h2>Invite somebody</h2>
      <form class="invite" onsubmit={(e) => { e.preventDefault(); invite(); }}>
        <input placeholder="Their name" bind:value={name} maxlength="40" />
        <button class="primary" disabled={!name.trim() || busy === 'invite'}>Invite</button>
      </form>
      <p class="note">
        The name is the room's key for them: it is what you remove them by, and it stays
        theirs however they rename themselves at the door. They start with the lobby and a
        room of their own; anything else is given below.
      </p>
      {#if fresh}
        <div class="link">
          <strong>{fresh.name}</strong>, {fresh.why}. The link works once, in the browser
          that opens it:
          <code>{full(fresh.link)}</code>
          <div class="link-do">
            <button onclick={() => copy(fresh.link)}>
              {copied === full(fresh.link) ? 'Copied' : 'Copy'}
            </button>
            <button onclick={() => (fresh = null)}>Done with it</button>
          </div>
        </div>
      {/if}
    </section>

    <section>
      <h2>Rooms</h2>
      <form class="invite" onsubmit={(e) => { e.preventDefault(); make(); }}>
        <input placeholder="A name for the room" bind:value={newRoom} maxlength="32" />
        <button class="primary" disabled={!newRoom.trim() || busy === 'make'}>Make a room</button>
      </form>
      <ul class="rooms">
        {#each house.allRooms as r (r.id)}
          <li>
            {#if naming?.id === r.id}
              <form class="rename" onsubmit={(e) => { e.preventDefault(); saveName(); }}>
                <input bind:value={naming.title} maxlength="32" aria-label="A new name for {r.title}" />
                <button class="primary">Save</button>
                <button type="button" onclick={() => (naming = null)}>Cancel</button>
              </form>
            {:else}
              <span class="what">
                <strong>{r.title}</strong>
                {#if r.id === house.lobby}<span class="tag">lobby</span>{/if}
              </span>
              <button onclick={() => startNaming(r)}>Rename</button>
              {#if r.id !== house.lobby}
                <button class="danger" disabled={busy === `drop-room:${r.id}`}
                  onclick={() => dropRoom(r)}>Take it away</button>
              {/if}
            {/if}
          </li>
        {/each}
      </ul>
      <p class="note">
        A new room is yours alone until you give it to somebody below. The lobby is where
        everybody lands and nobody can be refused, so it stays. Renaming a room changes
        nothing but the word: what was said in it, and who may go to it, are kept against
        the room itself and not against what it is called.
      </p>
    </section>

    <section>
      <h2>Everybody invited</h2>
      <!-- Named, because the page has two of these and they are not
           the same question: who may go where, and which AI is where. -->
      <table class="who">
        <thead>
          <tr>
            <th>Name</th>
            <th>Link</th>
            {#each given as r (r.id)}<th class="room">{r.title}</th>{/each}
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each house.people as p (p.name)}
            <tr class:me={p.name === house.you}>
              <td>
                <strong>{p.name}</strong>
                {#if p.owner}<span class="tag">owner</span>{/if}
                {#if p.display !== p.name}<small>in the room as {p.display}</small>{/if}
                {#if p.language}<small>{p.language}</small>{/if}
              </td>
              <td>
                {#if p.link}
                  <button onclick={() => copy(p.link)}>
                    {copied === full(p.link) ? 'Copied' : 'Copy link'}
                  </button>
                {:else}
                  <span class="note">has been in</span>
                {/if}
                <button disabled={busy === `link:${p.name}`} onclick={() => relink(p.name)}>
                  New link
                </button>
              </td>
              {#each given as r (r.id)}
                <td class="room">
                  <input type="checkbox" aria-label="{p.name} may go to {r.title}"
                    checked={p.owner || p.rooms.includes(r.id)}
                    disabled={p.owner || busy === `room:${p.name}:${r.id}`}
                    onchange={(e) => toggle(p.name, r.id, e.currentTarget.checked)} />
                </td>
              {/each}
              <td>
                {#if !p.owner}
                  <button class="danger" disabled={busy === `drop:${p.name}`}
                    onclick={() => remove(p.name)}>Remove</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="note">
        Everybody has the lobby and a room of their own; the columns are the rooms that are
        given out. The owner has the house, which is why those are ticked and fixed — and
        not a room of anybody else's, which nobody has but the person it belongs to.
      </p>
      <p class="note">
        To hand the house to somebody else, remove the owner at the server
        (<code>vox_room uninvite {house.you}</code>) and then
        <code>vox_room invite-owner NAME</code>. It is a command and not a button on purpose:
        it is the one thing nobody should be able to do by clicking the wrong row.
      </p>
    </section>

    <section>
      <h2>The AI voices</h2>
      <table class="voices">
        <thead>
          <tr>
            <th>Name</th>
            <th class="room">Lobby</th>
            {#each given as r (r.id)}<th class="room">{r.title}</th>{/each}
            <th class="room">Rooms of their own</th>
          </tr>
        </thead>
        <tbody>
          {#each house.bots as b (b.name)}
            <tr>
              <td>
                <strong>{b.name}</strong>
                {#if b.first}<span class="tag">the room's own</span>{/if}
              </td>
              {#each [{ id: house.lobby, title: 'the lobby' }, ...given, { id: house.own, title: 'a room of somebody\'s own' }] as r (r.id)}
                <td class="room">
                  <input type="checkbox" aria-label="{b.name} is in {r.title}"
                    checked={b.first || b.rooms.includes(r.id)}
                    disabled={b.first || busy === `bot:${b.name}:${r.id}`}
                    onchange={(e) => toggleBot(b.name, r.id, e.currentTarget.checked)} />
                </td>
              {/each}
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="note">
        Which rooms each AI is in. The first one is the room's own — it is what a room has,
        the way a room has a lobby — so its ticks are fixed; the others are somewhere
        because you put them there. A tick reaches the room at once: a fader appears or
        goes for everybody sitting in it, and an AI taken out of a room it was speaking in
        stops where it is. Nothing of what it said is lost, and putting it back brings it
        back to all of it.
      </p>
      <p class="note">
        <strong>Rooms of their own</strong> is one answer for every one of them, because a
        room of your own is yours: the AIs in it are the house's, not a thing to arrange
        person by person. The AIs come from how the server was started
        (<code>--bot</code>), not from this page: this is where they are, not whether they
        exist.
      </p>
    </section>
  {/if}
</main>

<style>
  .people {
    max-width: 60em;
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
  .note { color: var(--muted); font-size: 13px; line-height: 1.5; margin: 10px 0 0; }
  .warn { color: var(--danger); font-size: 13px; margin: 10px 0 0; }
  .invite, .rename { display: flex; gap: 8px; }
  .invite input { flex: 1; max-width: 20em; }
  .link {
    margin-top: 12px;
    padding: 10px 12px;
    border: 1px solid var(--accent);
    border-radius: 8px;
    font-size: 13px;
    line-height: 1.6;
  }
  .link code { display: block; margin: 6px 0; word-break: break-all; user-select: all; }
  .link-do { display: flex; gap: 8px; }
  .rooms { list-style: none; margin: 12px 0 0; padding: 0; font-size: 13px; }
  .rooms li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 0;
    border-top: 1px solid var(--line);
  }
  .rooms .what { flex: 1; }
  .rename { flex: 1; }
  .rename input { flex: 1; max-width: 20em; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; }
  th { text-align: left; font-weight: 600; color: var(--muted); padding: 4px 8px; }
  th.room, td.room { text-align: center; width: 1%; white-space: nowrap; }
  td { padding: 6px 8px; border-top: 1px solid var(--line); vertical-align: middle; }
  td button { margin-right: 4px; }
  small { display: block; color: var(--muted); }
  .tag {
    font-size: 11px;
    border: 1px solid var(--accent);
    color: var(--accent);
    border-radius: 999px;
    padding: 0 6px;
    margin-left: 4px;
  }
  kbd { font-size: 11px; opacity: 0.7; border: 1px solid currentColor; border-radius: 4px; padding: 0 4px; }
</style>
