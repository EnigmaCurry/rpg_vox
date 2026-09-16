<script>
  import { onMount } from 'svelte';
  import { route } from './lib/router.js';
  import {
    startHealthPoll,
    startRecordingPoll,
    reloadSettings,
    restorePwMonitorFromPref,
  } from './lib/stores.js';
  import { restoreFromPref as restoreBrowserMonitor } from './lib/browserMonitor.js';
  import Menubar from './components/Menubar.svelte';
  import Projects from './routes/Projects.svelte';
  import Characters from './routes/Characters.svelte';
  import Dictionary from './routes/Dictionary.svelte';
  import Scenes from './routes/Scenes.svelte';
  import Script from './routes/Script.svelte';
  import Mixer from './routes/Mixer.svelte';
  import Record from './routes/Record.svelte';
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
    '/dictionary': Dictionary,
    '/scenes':     Scenes,
    '/speak':      Scenes,
    '/script':     Script,
    '/chat':       Script,
    '/mixer':      Mixer,
    '/record':     Record,
    '/settings':   Settings,
  };

  /// Resolve the view for the current hash route. Supports parameterised
  /// paths — currently only /script/:id and its /chat alias. The parameter
  /// itself is read from `$route` inside Script.svelte via a helper, so
  /// the router doesn't need to hand it down separately.
  const View = $derived.by(() => {
    const r = $route;
    if (r === '/script' || r === '/chat') return Script;
    if (r.startsWith('/script/') || r.startsWith('/chat/')) return Script;
    return routes[r] ?? NotFound;
  });

  // The Scenes view is a full-bleed sidebar+lanes layout; other routes keep
  // the narrow centered column. Toggle a class on <main> to switch modes.
  // /script and /chat use their own bottom-locked layout so we let them run
  // full-bleed too rather than centering under the 720px cap.
  const isFullBleed = $derived(
    $route === '/scenes' || $route === '/speak'
      || $route === '/record' || $route === '/mixer'
      || $route.startsWith('/script') || $route.startsWith('/chat'),
  );
  // Record grows tall and uses the page scrollbar instead of an inner
  // one, unlike other full-bleed routes which cap at viewport height.
  const isPageScroll = $derived($route === '/record' || $route === '/mixer');

  onMount(() => {
    startHealthPoll();
    startRecordingPoll();
    // Prime the settings store so `$settings.node_name` is populated for
    // pages that reference it (Mixer's Input Nodes labels, etc.). Static
    // after startup, so no polling needed.
    reloadSettings().catch((err) => {
      console.warn('[settings] initial load failed', err);
    });
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

<main class:full-bleed={isFullBleed} class:page-scroll={isPageScroll}>
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
    /* Fill the viewport space left by the Menubar (body is a flex column).
       min-height:0 lets inner scrollers actually scroll instead of pushing
       body past 100vh, and overflow:hidden keeps the body scrollbar off. */
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  /* Record uses the *page* scrollbar as its only scrollbar (log grows
     tall and pushes body past 100vh instead of clipping into an inner
     scroller). The route-scoped override relaxes the bounds that other
     full-bleed routes rely on. */
  main.page-scroll {
    flex: 1 0 auto;
    overflow: visible;
    min-height: 0;
  }
</style>
