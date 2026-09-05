<script>
  import { health } from '../lib/stores.js';
  import { monitorState } from '../lib/browserMonitor.js';

  const label = $derived({ checking: 'checking…', ok: 'online', err: 'offline' }[$health] || '');
  // Only decorate with "streaming" when the app is healthy AND the browser
  // monitor is actively receiving (WS open, decoder configured). Any other
  // monitor state ('connecting', 'error', 'stopped') is quiet — no need to
  // clutter the menubar with transient states.
  const streaming = $derived($health === 'ok' && $monitorState === 'listening');
</script>

<span class="health {$health}">
  {label}{#if streaming}<span class="streaming"> · streaming</span>{/if}
</span>

<style>
  .health {
    font-size: 12px;
    color: var(--muted);
  }
  .health.ok  { color: var(--ok); }
  .health.err { color: var(--err); }
  .streaming {
    color: var(--accent);
    font-weight: 500;
  }
</style>
