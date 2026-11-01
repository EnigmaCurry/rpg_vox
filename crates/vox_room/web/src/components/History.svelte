<script>
  import { room, loadHistory, readConversation, backToNow, forgetConversation } from '../lib/room.svelte.js';

  // The bar over the conversation: which one you are reading, and the
  // way into the ones the room has had before.
  //
  // It sits here rather than in the top bar or the room list because
  // this is the column it governs — and because reading an old
  // conversation is a thing you do alone. Nothing in here is sent to
  // the room; the only exception is deleting one, which takes it away
  // from everybody and asks first.
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

  const when = (at) =>
    at
      ? new Date(at).toLocaleString([], {
          month: 'short',
          day: 'numeric',
          hour: '2-digit',
          minute: '2-digit',
        })
      : 'some time ago';

  const lines = (n) => `${n} ${n === 1 ? 'line' : 'lines'}`;
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
  {#if reading}
    <button class="back" onclick={backToNow}>← Back to the conversation</button>
    <span class="closed">
      {#if reading.game}<strong>{reading.game}</strong> ·{/if}
      This one is closed — nothing can be said into it.
    </span>
  {:else}
    <span class="now">{room.game ?? 'The conversation'}</span>
  {/if}

  <div class="spacer"></div>

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
                <span class="at">{when(c.at)}</span>
                {#if c.live}<span class="tag">now</span>{/if}
                {#if c.game}<span class="tag game">{c.game}</span>{/if}
                <span class="meta">{lines(c.lines)}</span>
              </span>
              <span class="opening">{c.opening || 'Nothing was said.'}</span>
            </button>
            <!-- The live one is the room, not an archive of it. -->
            {#if !c.live}
              <button class="drop" title="Delete this conversation for good"
                aria-label="Delete the conversation from {when(c.at)}"
                onclick={() => forgetConversation(c.id)}>×</button>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .bar {
    position: sticky;
    top: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 10px;
    /* Stuck to the head of the scroller, so what passes behind it is
       covered rather than showing through. */
    background: var(--bg);
    padding: 2px 0 8px;
    margin-bottom: 2px;
    border-bottom: 1px solid var(--line);
  }
  .spacer { flex: 1; }
  .now { font-size: 12px; color: var(--muted); }
  .closed { font-size: 12px; color: var(--muted); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .back { padding: 2px 10px; font-size: 13px; }
  .picker { position: relative; }
  .picker > button { padding: 2px 10px; font-size: 13px; }
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
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: var(--shadow), 0 10px 30px rgb(0 0 0 / 12%);
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
    color: var(--muted);
    font-size: 16px;
    line-height: 1;
    padding: 4px 9px;
    border-radius: 6px;
  }
  .drop:hover { background: var(--danger); color: #fff; }
</style>
