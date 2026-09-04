<script>
  import { route } from '../lib/router.js';
  import HealthDot from './HealthDot.svelte';

  const items = [
    { href: '#/scenes',   label: 'Scenes',   match: (r) => r === '/' || r === '/scenes' || r === '/speak' },
    { href: '#/chat',     label: 'Chat',     match: (r) => r === '/chat' },
    { href: '#/settings', label: 'Settings', match: (r) => r === '/settings' },
  ];
</script>

<nav class="menubar">
  <a class="brand" href="#/">RPG Vox</a>
  <ul>
    {#each items as it (it.href)}
      <li>
        <a href={it.href} class:active={it.match($route)}>{it.label}</a>
      </li>
    {/each}
  </ul>
  <HealthDot />
</nav>

<style>
  .menubar {
    display: flex;
    align-items: center;
    gap: 20px;
    padding: 10px 20px;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
    position: sticky;
    top: 0;
    z-index: 10;
  }
  .brand {
    font-weight: 700;
    color: var(--text);
    text-decoration: none;
    font-size: 15px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    gap: 4px;
    flex: 1;
  }
  li a {
    display: inline-block;
    padding: 6px 10px;
    color: var(--muted);
    text-decoration: none;
    border-radius: 6px;
    font-size: 14px;
  }
  li a:hover { color: var(--text); background: rgba(255,255,255,0.04); }
  li a.active {
    color: var(--text);
    background: rgba(122,162,255,0.12);
    box-shadow: inset 0 -2px 0 var(--accent);
  }
</style>
