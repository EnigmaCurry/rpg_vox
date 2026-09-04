<script>
  import { onMount } from 'svelte';
  import { route } from './lib/router.js';
  import { startHealthPoll } from './lib/stores.js';
  import Menubar from './components/Menubar.svelte';
  import Speak from './routes/Speak.svelte';
  import Chat from './routes/Chat.svelte';
  import Settings from './routes/Settings.svelte';
  import NotFound from './routes/NotFound.svelte';

  // Add new experiments here — each is a hash-URL entry mapped to a component.
  const routes = {
    '/':         Speak,
    '/speak':    Speak,
    '/chat':     Chat,
    '/settings': Settings,
  };

  const View = $derived(routes[$route] ?? NotFound);

  onMount(() => { startHealthPoll(); });
</script>

<Menubar />

<main>
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
</style>
