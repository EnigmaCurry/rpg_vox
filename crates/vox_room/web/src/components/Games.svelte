<script>
  import { room, command, ask, enterGame, peekGame } from '../lib/room.svelte.js';

  // The games live in the top bar: a menu of what is saved, and a card
  // for whichever one you pick.
  let open = $state(false);
  let menu = $state();
  let shown = $state(null);
  let dialog = $state();

  // Follow the saved games, so a card stays right while the AI revises
  // or deletes the game behind it.
  const game = $derived(shown && room.games.find((g) => g.name === shown));
  const playing = $derived(game && room.game === game.name);

  $effect(() => {
    if (!dialog) return;
    if (game && !dialog.open) dialog.showModal();
    if (!game && dialog.open) dialog.close();
  });

  export function players(g) {
    const { min, max } = g.settings?.players ?? {};
    if (min && max) return min === max ? `${min} players` : `${min}–${max} players`;
    if (min) return `${min} or more players`;
    if (max) return `up to ${max} players`;
    return 'any number of players';
  }

  const here = $derived(room.people.filter((p) => p.online).length);

  function pick(name) {
    open = false;
    shown = name;
  }

  async function play() {
    const name = game.name;
    shown = null;
    await enterGame(name);
  }

  async function remove() {
    const name = game.name;
    if (await ask(`Delete ${name} for good? This can't be undone.`, 'Delete')) {
      shown = null;
      command({ type: 'delete_game', name });
    }
  }
</script>

<svelte:window onclick={(e) => { if (open && !menu?.contains(e.target)) open = false; }}
  onkeydown={(e) => { if (e.key === 'Escape') open = false; }} />

<div class="games" bind:this={menu}>
  <button aria-haspopup="menu" aria-expanded={open} class:on={open}
    onclick={() => (open = !open)}>
    Games
    {#if room.games.length}<span class="count">{room.games.length}</span>{/if}
    <span class="caret" aria-hidden="true">▾</span>
  </button>

  {#if open}
    <div class="menu" role="menu">
      {#if room.games.length === 0}
        <p class="none">None yet. Ask {room.agent.name} to design one with you.</p>
      {/if}
      {#each room.games as g (g.name)}
        <button role="menuitem" class="item" class:current={room.game === g.name}
          onclick={() => pick(g.name)}>
          <span class="name">
            {g.name}
            {#if room.game === g.name}<span class="tag">playing</span>{/if}
          </span>
          <span class="meta">{players(g)}</span>
        </button>
      {/each}
    </div>
  {/if}
</div>

<dialog class="ask card" bind:this={dialog}
  oncancel={(e) => { e.preventDefault(); shown = null; }}
  onclick={(e) => { if (e.target === dialog) shown = null; }}>
  {#if game}
    <h2>{game.name}</h2>
    <p class="meta">
      <span>{players(game)}</span>
      <span class="dot">·</span>
      <span>{here} here now</span>
      {#if game.settings?.reply && game.settings.reply !== 'room'}
        <span class="dot">·</span>
        <span>{game.settings.reply === 'always'
          ? `${room.agent.name} answers every pause`
          : game.settings.reply === 'auto'
            ? `${room.agent.name} judges when to speak`
            : `${room.agent.name} speaks only when named`}</span>
      {/if}
      <span class="dot">·</span>
      <span>{game.played ? 'played before' : 'never played'}</span>
    </p>
    <p class="summary">{game.summary}</p>
    {#if playing}
      <p class="note">You're playing this now. <strong>Restart game</strong> starts it over.</p>
    {:else if room.game}
      <p class="note">You're in {room.game}. Go back to the lobby to play another game.</p>
    {/if}
    <div class="ask-actions">
      <button class="danger" onclick={remove} disabled={playing}
        title={playing ? "It's being played: go back to the lobby first." : `Delete ${game.name} for good`}>Delete</button>
      <button onclick={() => peekGame(game.name)}
        title="Show the game's hidden prompt, to you only">Peek</button>
      <button onclick={() => (shown = null)}>Close</button>
      <button class="primary" onclick={play} disabled={!!room.game}>Play</button>
    </div>
  {/if}
</dialog>

<style>
  .games { position: relative; }
  .count {
    font-size: 11px;
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 0 5px;
    margin-left: 2px;
  }
  .caret { font-size: 10px; opacity: 0.7; margin-left: 2px; }
  button.on { border-color: var(--muted); }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 20;
    min-width: 17em;
    max-width: min(26em, 90vw);
    max-height: 60vh;
    overflow-y: auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: var(--shadow), 0 10px 30px rgb(0 0 0 / 12%);
    padding: 4px;
  }
  .none { font-size: 13px; color: var(--muted); margin: 0; padding: 8px 10px; }
  .item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    width: 100%;
    text-align: left;
    border: 0;
    border-radius: 7px;
    padding: 7px 10px;
    background: transparent;
  }
  .item:hover { background: var(--bubble); }
  .item.current .name { color: var(--agent); }
  .name { font-weight: 600; display: flex; align-items: center; gap: 6px; }
  .meta { font-size: 12px; color: var(--muted); }
  .tag {
    font-size: 11px;
    font-weight: 500;
    color: var(--agent);
    border: 1px solid currentColor;
    border-radius: 999px;
    padding: 0 6px;
  }
  /* The card for one game. */
  .card { width: min(32em, calc(100vw - 32px)); max-width: none; }
  .card .ask-actions { margin-top: 2px; }
  .card h2 { font-size: 20px; text-transform: none; letter-spacing: 0; color: inherit; margin: 0 0 6px; }
  .card .meta { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 12px; }
  .card .dot { opacity: 0.5; }
  .summary { margin: 0 0 14px !important; }
  .note { font-size: 13px; color: var(--muted); margin: 0 0 14px !important; }
  .ask-actions .danger { margin-right: auto; }
</style>
