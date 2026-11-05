<script>
  import { room, sendText } from '../lib/room.svelte.js';
  import { TYPED_MAX } from '../lib/names.js';

  // The box you type into, along the foot of the room beside the
  // button you talk with. Saying it and typing it are the same act
  // done two ways, so they are the same width of screen apart; and the
  // conversation above has the rows the box used to take.
  //
  // A box of one line that grows, rather than a box of several that
  // mostly sits empty: most messages are a line, and the ones that
  // aren't say so by being typed.
  let draft = $state('');
  let box = $state();

  // No taller than half the window, after which it scrolls. Measured
  // from nothing each time, because a box that has only ever been told
  // to grow cannot shrink again when the text is deleted.
  function fit() {
    if (!box) return;
    box.style.height = 'auto';
    box.style.height = `${Math.min(box.scrollHeight, window.innerHeight / 2)}px`;
  }
  // Every keystroke, and every change of window: the draft is what is
  // being measured, so it has to be read here or this never runs again
  // after the first paint.
  $effect(() => {
    draft;
    fit();
  });

  function send(ev) {
    ev?.preventDefault();
    if (sendText(draft)) draft = '';
  }

  // Enter sends, because that is what it does everywhere a message is
  // typed. Shift holds it back for the line break, which is the only
  // way to get one in a box this is the whole of.
  function onkeydown(e) {
    if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    send();
  }
</script>

<!-- A window that got shorter can leave the box taller than half of
     it, which is the one thing it must never be. -->
<svelte:window onresize={fit} />

<!-- Nothing to type into a conversation the room has finished with:
     there is nowhere for what you typed to go. -->
{#if !room.reading}
  <form class="say" onsubmit={send}>
    <textarea
      bind:this={box}
      bind:value={draft}
      rows="1"
      maxlength={TYPED_MAX}
      autocomplete="off"
      placeholder="Type your message here ..."
      aria-label="Type a message to the room"
      {onkeydown}
    ></textarea>
    <!-- Only once there is something to send: an empty box has nothing
         to press this for, and the room is typed into with Enter. -->
    {#if draft.trim()}<button type="submit">Send</button>{/if}
  </form>
{/if}

<style>
  /* The bar is the field, as it was at the foot of the conversation:
     nothing round, no border of its own, and the whole of it lights up
     when it has the focus. */
  .say {
    /* A basis wide enough to type in, so that a footer with no room
       left for it wraps it onto a line of its own. Flexed from zero it
       is never too big to fit, and a narrow window leaves it a
       character wide and a half-screen tall. Wide enough that it wraps
       while there is still a line of room in it, rather than after
       being squeezed down to a word. */
    flex: 1 1 20em;
    min-width: 0;
    display: flex;
    align-items: flex-end;
    gap: 10px;
    border: 1px solid var(--field-border);
    /* The same corner as the buttons it shares the footer with. */
    border-radius: var(--field-radius);
    background: var(--field-bg);
    padding: 0 10px;
  }
  .say:focus-within { border-color: var(--accent); box-shadow: var(--field-focus); }
  textarea {
    flex: 1;
    min-width: 0;
    /* `auto` while it is measured, then whatever it measured. */
    height: auto;
    max-height: 50vh;
    resize: none;
    overflow-y: auto;
    overscroll-behavior: contain;
    font: inherit;
    line-height: 1.4;
    color: var(--ink);
    background: none;
    border: 0;
    padding: 9px 0;
  }
  textarea:focus { outline: none; }
  textarea::placeholder { color: var(--muted); }
  /* A word at the end of the bar rather than a button on top of it:
     Enter already does this, so it carries no weight of its own. */
  button {
    flex: none;
    border: 0;
    border-radius: 0;
    background: none;
    box-shadow: none;
    color: var(--accent);
    font-weight: 600;
    padding: 9px 0;
  }
  button:hover { color: var(--ink); background: none; }
</style>
