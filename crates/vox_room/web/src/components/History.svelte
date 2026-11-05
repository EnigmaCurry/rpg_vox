<script>
  import {
    room, ask, command, loadHistory, readConversation, backToNow, forgetConversation,
    nameConversation, whenItWas,
  } from '../lib/room.svelte.js';
  import { TITLE_MAX } from '../lib/names.js';

  // The bar over the conversation: which one you are reading, what the
  // room is playing, and the way into the ones it has had before.
  //
  // Everything here is about this conversation, which is why it is
  // here: there is no title bar over the room any more, and what used
  // to be in it has gone to whichever column it was actually about.
  // Starting a new conversation and leaving a game are both of them
  // this one, so they are the two things in here that the whole room
  // sees — and both ask first, because both take something away from
  // everybody.
  let open = $state(false);
  let menu = $state();

  const reading = $derived(room.reading);

  async function show() {
    open = !open;
    // Re-read each time it is opened: the room has been talking while
    // this was shut, and may well have started another one.
    if (open) await loadHistory();
  }

  async function pick(c) {
    open = false;
    await readConversation(c.id);
  }

  const lines = (n) => `${n} ${n === 1 ? 'line' : 'lines'}`;

  // What to call one: what somebody called it, or when it was.
  const named = (c) => c.title || whenItWas(c);

  // The box at the head of whichever conversation you have open — the
  // one the room is having, or one out of its history. An old one is
  // read-only in the sense that matters, which is that nothing can be
  // *said* into it; a name is not something said, and being able to
  // name one you forgot to name is most of what naming is for.
  //
  // While you are typing in it the box is yours; the moment you are
  // not, it is the room's again — so a name somebody else gives it
  // doesn't change under your hands mid-word.
  let typing = $state(null);
  const showing = $derived(
    reading
      ? { id: reading.id, title: reading.title ?? '', at: reading.at, said: reading.log?.[0]?.at }
      : {
          id: room.conversation.id,
          title: room.conversation.title,
          at: room.conversation.at,
          said: room.log.find((e) => e.at)?.at,
        },
  );
  const mine = $derived(typing ?? (showing.title || whenItWas(showing)));

  async function newConversation() {
    const question = room.game
      ? `Restart ${room.game} from the beginning, for everyone?`
      : 'Start a new conversation? The current one is kept in the database but cleared from the screen.';
    if (await ask(question, room.game ? 'Restart' : 'New conversation')) command({ type: 'new_conversation' });
  }

  // "Leave the game", not "back to the lobby": a room may well be
  // called Lobby, and a button that reads like it goes there when it
  // does nothing of the kind is a worse button than a plain one.
  async function lobby() {
    if (await ask(`Leave ${room.game} and take everyone in this room back to the conversation?`, 'Leave the game')) {
      command({ type: 'lobby' });
    }
  }

  function rename() {
    if (typing === null) return;
    const title = typing.trim();
    typing = null;
    // Typing the default back in is not naming it that: leave it
    // unnamed, so it goes on following what is in it.
    nameConversation(showing.id, title === mine && !showing.title ? '' : title);
  }
</script>

<!-- A click in a dialog is not a click away from the list: confirming a
     delete would otherwise shut the list you were deleting from.
     
     On the way down, not the way up. The dialog's buttons only exist
     while there is a question, so answering one takes the button out of
     the page — and by the time a bubbling click reached here its target
     would be an orphan, with no dialog above it to find. -->
<svelte:window onclickcapture={(e) => {
    if (open && !menu?.contains(e.target) && !e.target.closest?.('dialog')) open = false;
  }}
  onkeydown={(e) => { if (e.key === 'Escape') open = false; }} />

<div class="bar">
  {#if !room.mayTranscribe}
    <!-- A room that is never written down has no conversation to name,
         none to start, and none to read back: there is nothing for any
         of the three controls to be about. What is left is the one
         sentence that says why the column below is empty. -->
    <span class="closed">Nothing here is written down.</span>
    <div class="spacer"></div>
    <span class="hatch mark" aria-hidden="true"></span>
  {:else}
    {#if reading}<button class="back" onclick={backToNow}>← Back</button>{/if}

    <!-- What this conversation is called, whichever one you have open.
         Unnamed, it is when it was; type over it and it is whatever you
         say, for everyone, and the room's history keeps it. -->
    <input class="name" value={mine} maxlength={TITLE_MAX}
      aria-label="What to call this conversation"
      title="What to call this conversation. Everyone here sees it."
      oninput={(e) => (typing = e.currentTarget.value)}
      onblur={rename}
      onkeydown={(e) => {
        if (e.key === 'Enter') { e.preventDefault(); e.currentTarget.blur(); }
        if (e.key === 'Escape') { typing = null; e.currentTarget.blur(); }
      }} />

    {#if reading}
      <span class="closed">
        {#if reading.game}{reading.game} · {/if}closed, nothing can be said into it
      </span>
    {:else if room.game}
      <span class="playing">{room.game}</span>
      <button class="leave" onclick={lobby}>Leave the game</button>
    {/if}

    <div class="spacer"></div>

    <!-- The console admitting it has a corner. Decoration, hidden from
         anything that reads the page out, and given no size at all by a
         theme that does not decorate. -->
    <span class="hatch mark" aria-hidden="true"></span>

    <!-- Starting the next one, beside the way back into the last ones:
         they are the same question asked in two directions. -->
    {#if !reading}
      <button class="fresh" onclick={newConversation}>
        {room.game ? 'Restart game' : 'New conversation'}
      </button>
    {/if}

    <div class="picker" bind:this={menu}>
      <button aria-haspopup="menu" aria-expanded={open} class:on={open} onclick={show}>
        History <span class="caret" aria-hidden="true">▾</span>
      </button>
      {#if open}
        <div class="menu" role="menu">
          {#if room.history === null}
            <p class="none">Reading…</p>
          {:else if room.history.length === 0}
            <p class="none">This room hasn't had a conversation yet.</p>
          {/if}
          {#each room.history ?? [] as c (c.id)}
            <div class="row" class:current={reading ? reading.id === c.id : c.live}>
              <button role="menuitem" class="item" onclick={() => pick(c)}>
                <span class="head">
                  <span class="at">{named(c)}</span>
                  {#if c.live}<span class="tag">now</span>{/if}
                  {#if c.game}<span class="tag game">{c.game}</span>{/if}
                  <span class="meta">{lines(c.lines)}</span>
                </span>
                <span class="opening">{c.opening || 'Nothing was said.'}</span>
              </button>
              <!-- The live one is the room, not an archive of it. -->
              {#if !c.live}
                <button class="drop" title="Delete this conversation for good"
                  aria-label="Delete {named(c)}"
                  onclick={() => forgetConversation(c.id)}>×</button>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .bar {
    position: sticky;
    /* Not flush with the top of the scroller: a framed theme draws a
       rail along the head of the column, and a bar stuck at 0 sits on
       top of it. Four pixels is the rail. */
    top: 4px;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 10px;
    /* Stuck to the head of the scroller, so what passes behind it is
       covered rather than showing through. */
    background: var(--well);
    padding: 2px 0 8px;
    margin-bottom: 2px;
    border-bottom: 1px solid var(--line);
  }
  .spacer { flex: 1; }
  .mark { flex: none; width: var(--hatch-w); height: var(--hatch-h); }
  /* A box that reads as the heading it is until you are in it. */
  .name {
    flex: 0 1 22em;
    min-width: 6em;
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    color: var(--ink);
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--field-radius);
    padding: 2px 6px;
  }
  /* Narrow: the name is what this bar is for -- it is the heading and
     it is the box you rename the conversation in -- so it gets what is
     left after the buttons, and the console's decorative corner, which
     is only ever decoration, stops competing with it for the width. */
  @media (max-width: 860px) {
    .bar { gap: 6px; }
    .mark { display: none; }
    .name { flex: 1 1 auto; min-width: 0; }
  }
  .name:hover { border-color: var(--line); }
  .name:focus { border-color: var(--accent); outline: none; background: var(--field-bg); }
  .playing {
    font-size: 11px;
    font-weight: 600;
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--agent);
    color: var(--on-solid);
  }
  .closed { font-size: 12px; color: var(--muted); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .back { padding: 2px 10px; font-size: 13px; }
  .fresh, .leave { padding: 2px 10px; font-size: 13px; white-space: nowrap; }
  .picker { position: relative; }
  .picker > button { padding: 2px 10px; font-size: 13px; white-space: nowrap; }
  .picker > button.on { border-color: var(--muted); }
  .caret { font-size: 10px; opacity: 0.7; }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 20;
    width: min(30em, 80vw);
    max-height: 60vh;
    overflow-y: auto;
    overscroll-behavior: contain;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: var(--shadow);
    padding: 4px;
  }
  .none { font-size: 13px; color: var(--muted); margin: 0; padding: 8px 10px; }
  .row { display: flex; align-items: stretch; gap: 2px; border-radius: 7px; }
  .row:hover { background: var(--bubble); }
  .row.current .at { color: var(--accent); }
  .item {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    text-align: left;
    border: 0;
    border-radius: 7px;
    padding: 7px 10px;
    background: transparent;
    box-shadow: none;
  }
  .head { display: flex; align-items: center; gap: 7px; flex-wrap: wrap; }
  .at { font-weight: 600; font-size: 13px; }
  .meta { font-size: 12px; color: var(--muted); }
  .tag {
    font-size: 11px;
    border-radius: 999px;
    padding: 0 6px;
    border: 1px solid currentColor;
    color: var(--accent);
  }
  .tag.game { color: var(--agent); }
  .opening {
    font-size: 12px;
    color: var(--muted);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .drop {
    flex: none;
    align-self: center;
    border: 0;
    background: transparent;
    box-shadow: none;
    color: var(--muted);
    font-size: 16px;
    line-height: 1;
    padding: 4px 9px;
    border-radius: 6px;
  }
  .drop:hover { background: var(--danger); color: var(--on-solid); }
</style>
