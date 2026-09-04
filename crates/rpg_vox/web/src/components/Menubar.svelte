<script>
  import { route } from '../lib/router.js';
  import { scenesState } from '../lib/scenes.svelte.js';
  import HealthDot from './HealthDot.svelte';

  const items = [
    { href: '#/projects',   label: 'Projects',   match: (r) => r === '/' || r === '/projects' },
    { href: '#/characters', label: 'Characters', match: (r) => r === '/characters' },
    { href: '#/scenes',     label: 'Scenes',     match: (r) => r === '/scenes' || r === '/speak' },
    { href: '#/chat',       label: 'Chat',       match: (r) => r === '/chat' },
    { href: '#/settings',   label: 'Settings',   match: (r) => r === '/settings' },
  ];

  const currentProject = $derived(
    scenesState.selectedProjectId
      ? scenesState.projects.find((p) => p.id === scenesState.selectedProjectId) ?? null
      : null,
  );
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
  <div class="status">
    <a class="project" href="#/projects" title="Change project">
      {#if currentProject}
        <span class="proj-label">Project</span>
        <span class="proj-name">{currentProject.name}</span>
      {:else}
        <span class="proj-none">No project loaded</span>
      {/if}
    </a>
    <HealthDot />
  </div>
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
  .status {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .project {
    display: flex;
    align-items: center;
    gap: 6px;
    text-decoration: none;
    padding: 4px 8px;
    border-radius: 6px;
    max-width: 240px;
    color: var(--text);
    font-size: 13px;
  }
  .project:hover { background: rgba(255,255,255,0.04); }
  .proj-label {
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    font-size: 10px;
    font-weight: 600;
  }
  .proj-name {
    font-weight: 500;
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .proj-none {
    color: var(--muted);
    font-style: italic;
    font-size: 12px;
  }
</style>
