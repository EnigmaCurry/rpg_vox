<script>
  import { onMount } from 'svelte';
  import { route } from './lib/router.js';
  import { startHealthPoll } from './lib/stores.js';
  import { restoreFromPref as restoreBrowserMonitor } from './lib/browserMonitor.js';
  import Menubar from './components/Menubar.svelte';
  import Projects from './routes/Projects.svelte';
  import Characters from './routes/Characters.svelte';
  import Scenes from './routes/Scenes.svelte';
  import Chat from './routes/Chat.svelte';
  import Settings from './routes/Settings.svelte';
  import NotFound from './routes/NotFound.svelte';

  // Add new experiments here — each is a hash-URL entry mapped to a component.
  // /speak stays as an alias so old bookmarks land on the same page.
  const routes = {
    '/':           Projects,
    '/projects':   Projects,
    '/characters': Characters,
    '/scenes':     Scenes,
    '/speak':      Scenes,
    '/chat':       Chat,
    '/settings':   Settings,
  };

  const View = $derived(routes[$route] ?? NotFound);

  // The Scenes view is a full-bleed sidebar+lanes layout; other routes keep
  // the narrow centered column. Toggle a class on <main> to switch modes.
  const isFullBleed = $derived(
    $route === '/scenes' || $route === '/speak',
  );

  onMount(() => {
    startHealthPoll();
    // Auto-restore the browser monitor if the user had it enabled in a
    // previous session. Deferred to the first user gesture inside the
    // helper if the browser blocks AudioContext creation.
    restoreBrowserMonitor().catch((err) => {
      console.warn('[monitor] restore failed', err);
    });
  });
</script>

<Menubar />

<main class:full-bleed={isFullBleed}>
  {#key $route}
    <View />
  {/key}
</main>

<style>
  main {
    max-width: 720px;
    margin: 0 auto;
    padding: 24px 20px 64px;
  }
  main.full-bleed {
    max-width: none;
    margin: 0;
    padding: 0;
  }
</style>
