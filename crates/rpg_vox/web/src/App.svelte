<script>
  import { onMount } from 'svelte';
  import { route } from './lib/router.js';
  import { startHealthPoll, restorePwMonitorFromPref } from './lib/stores.js';
  import { restoreFromPref as restoreBrowserMonitor } from './lib/browserMonitor.js';
  import Menubar from './components/Menubar.svelte';
  import Projects from './routes/Projects.svelte';
  import Characters from './routes/Characters.svelte';
  import Scenes from './routes/Scenes.svelte';
  import Script from './routes/Script.svelte';
  import Mixer from './routes/Mixer.svelte';
  import Settings from './routes/Settings.svelte';
  import NotFound from './routes/NotFound.svelte';

  // Add new experiments here — each is a hash-URL entry mapped to a component.
  // /speak stays as an alias so old bookmarks land on the same page.
  // /chat aliases to /script so pre-rename bookmarks still land on the new
  // conversation page.
  const routes = {
    '/':           Projects,
    '/projects':   Projects,
    '/characters': Characters,
    '/scenes':     Scenes,
    '/speak':      Scenes,
    '/script':     Script,
    '/chat':       Script,
    '/mixer':      Mixer,
    '/settings':   Settings,
  };

  const View = $derived(routes[$route] ?? NotFound);

  // The Scenes view is a full-bleed sidebar+lanes layout; other routes keep
  // the narrow centered column. Toggle a class on <main> to switch modes.
  // /script and /chat use their own bottom-locked layout so we let them run
  // full-bleed too rather than centering under the 720px cap.
  const isFullBleed = $derived(
    $route === '/scenes' || $route === '/speak'
      || $route === '/script' || $route === '/chat',
  );

  onMount(() => {
    startHealthPoll();
    // Auto-restore the browser monitor if the user had it enabled in a
    // previous session. Deferred to the first user gesture inside the
    // helper if the browser blocks AudioContext creation.
    restoreBrowserMonitor().catch((err) => {
      console.warn('[monitor] restore failed', err);
    });
    // Auto-restore the pipewire monitor selection so the chosen output sink
    // hears rpg_vox from app boot rather than only after visiting Settings.
    restorePwMonitorFromPref().catch((err) => {
      console.warn('[pw-monitor] restore failed', err);
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
