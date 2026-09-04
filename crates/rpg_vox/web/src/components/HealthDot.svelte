<script>
  import { health } from '../lib/stores.js';

  const label = $derived({ checking: 'checking…', ok: 'online', err: 'offline' }[$health] || '');
</script>

<span class="health {$health}">{label}</span>

<style>
  .health {
    font-size: 12px;
    color: var(--muted);
  }
  .health.ok  { color: var(--ok); }
  .health.err { color: var(--err); }
</style>
