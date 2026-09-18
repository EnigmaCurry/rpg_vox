<script>
  import { onMount } from 'svelte';
  import { route } from './lib/router.js';
  import {
    startHealthPoll,
    startRecordingPoll,
    reloadSettings,
    restorePwMonitorFromPref,
    reloadGraph,
    graph,
    getClientId,
  } from './lib/stores.js';
  import {
    restoreFromPref as restoreBrowserMonitor,
    isPrefEnabled as isMonitorPrefEnabled,
    stop as stopBrowserMonitor,
  } from './lib/browserMonitor.js';
  import {
    isSupported as browserMicIsSupported,
    restoreFromPref as restoreBrowserMic,
    isPrefEnabled as isMicPrefEnabled,
    armGestureCapture as armMicGestureCapture,
    disarmGestureCapture as disarmMicGestureCapture,
    stopPresence as stopBrowserMic,
    micState,
  } from './lib/browserMic.js';
  import {
    initialAncillaryCheck,
    audioAncillary,
    setMonitorHasAudio,
    setMicHasAudio,
  } from './lib/audioClaim.js';

  const clientId = getClientId();
  const browserMicSupported = browserMicIsSupported();
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
    // Web-audio claim coordination. Before restoring monitor / mic
    // prefs, check whether another tab in this browser + origin is
    // already holding audio. If yes, we boot as an ancillary session:
    // Web audio prefs stay off, the mute button hides, but every
    // other control on the page remains fully usable (Sources
    // routing, mixer sliders, chat, record UIs, etc.). Non-ancillary
    // tabs proceed with the normal restore path.
    initialAncillaryCheck().then((ancillary) => {
      if (ancillary) {
        console.info('[audio] ancillary session — another tab is holding web audio; skipping restore');
        // Still fetch the graph once so Sources rows appear and
        // routing controls are usable.
        reloadGraph().catch(() => {});
        return;
      }
      // Immediately publish an intent claim based on the persisted
      // prefs — BEFORE the WebSockets actually open. Without this, a
      // second tab loading within ~500 ms of us would run its own
      // ancillary check while our monitor is still in 'connecting'
      // (myHasAudio still false), miss the claim, and boot as a
      // second primary. Publishing intent up-front closes that race:
      // any tab whose hello arrives after this line sees hasAudio=true
      // in our `here` reply.
      if (isMonitorPrefEnabled()) setMonitorHasAudio(true);
      if (isMicPrefEnabled())     setMicHasAudio(true);
      // Auto-restore the browser monitor if the user had it enabled
      // in a previous session. Deferred to the first user gesture
      // inside the helper if the browser blocks AudioContext creation.
      restoreBrowserMonitor().catch((err) => {
        console.warn('[monitor] restore failed', err);
      });
      // Web microphone presence restore. Opens /mic.ws so the server
      // can re-seed the slot's routing from its persisted pref; the
      // gesture-triggered auto-start elsewhere picks up capture on
      // the first user click. A one-shot graph refresh after presence
      // is up populates `$graph.web_mic_sources` for the app-level
      // effect below.
      restoreBrowserMic()
        .then(() => reloadGraph().catch(() => {}))
        .catch((err) => {
          console.warn('[mic] restore failed', err);
        });
    });
    // Auto-restore the pipewire monitor selection so the chosen
    // output sink hears rpg_vox from app boot rather than only after
    // visiting Settings. Not gated by ancillary state — pipewire
    // routing is a global server-side setting, not a per-tab audio
    // stream.
    restorePwMonitorFromPref().catch((err) => {
      console.warn('[pw-monitor] restore failed', err);
    });
  });

  // If we become ancillary after boot — either because a peer with a
  // smaller tabId shows up in a boot race, or because another tab
  // takes over audio while we were idle — release both audio paths.
  // This is what makes the boot race self-heal: both racing tabs may
  // initially decide "primary", but the moment they see each other,
  // the deterministic tie-break puts one of them into ancillary state
  // and this effect tears down its now-forbidden audio.
  let wasAncillary = false;
  $effect(() => {
    const now = $audioAncillary;
    if (now && !wasAncillary) {
      stopBrowserMonitor().catch(() => {});
      stopBrowserMic().catch(() => {});
    }
    wasAncillary = now;
  });

  // App-level auto-resume for the web microphone. Lives here rather than
  // in Mixer.svelte so it works from any route — the user can reload on
  // /script or /record with a persisted mic routing and the first click
  // anywhere still resumes capture. Watches this browser's own web-mic
  // row in $graph and toggles the gesture listener based on whether we
  // have a routing pinned but haven't captured yet.
  $effect(() => {
    if (!browserMicSupported) return;
    // Ancillary tabs must never arm the capture-resume listener.
    // Otherwise the first user click (including a Sources routing
    // click) triggers `startCapture()`, which broadcasts
    // `hasAudio: true` and flips the primary tab into ancillary via
    // the tabId tie-break — swapping the roles in the middle of a
    // user interaction and leaving both tabs showing the "another
    // browser tab is holding web audio" warning.
    if ($audioAncillary) {
      disarmMicGestureCapture();
      return;
    }
    const own = ($graph?.web_mic_sources ?? []).find((w) => w.client_uuid === clientId);
    const needsGesture = !!own?.routed_to
      && $micState.capture !== 'active'
      && $micState.capture !== 'starting';
    if (needsGesture) armMicGestureCapture();
    else              disarmMicGestureCapture();
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
