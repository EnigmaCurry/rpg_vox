<script>
  import { health, recordingStatus } from '../lib/stores.js';
  import { monitorState } from '../lib/browserMonitor.js';

  const label = $derived({ checking: 'checking…', ok: 'online', err: 'offline' }[$health] || '');
  // Only decorate with "streaming" when the app is healthy AND the browser
  // monitor is actively receiving (WS open, decoder configured). Any other
  // monitor state ('connecting', 'error', 'stopped') is quiet — no need to
  // clutter the menubar with transient states.
  const streaming = $derived($health === 'ok' && $monitorState === 'listening');

  // When a recording is active, the health chip is replaced by a red
  // pulsing dot + "RECORDING" + running duration (hh:mm). Tracks the
  // wall clock locally so the timer advances smoothly between the 1s
  // recordingStatus polls instead of ticking in visible 1s hops.
  const recording = $derived($recordingStatus?.active === true);

  let now = $state(Date.now());
  let clockTimer = null;
  $effect(() => {
    if (recording) {
      clockTimer = setInterval(() => { now = Date.now(); }, 1000);
      return () => { clearInterval(clockTimer); clockTimer = null; };
    }
  });

  function fmtHhMm(ms) {
    if (!Number.isFinite(ms) || ms < 0) ms = 0;
    const totalMinutes = Math.floor(ms / 60000);
    const h = Math.floor(totalMinutes / 60);
    const m = totalMinutes % 60;
    return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
  }

  // Recording elapsed = last-known server duration (from the 1s poll)
  // plus whatever wall-clock time has passed since we received it.
  const elapsedMs = $derived.by(() => {
    if (!recording) return 0;
    // The poll gives us a rough duration; interpolate off `now` so the
    // display isn't visibly quantized to 1-second poll ticks. Fall back
    // to server duration if `startedAt` isn't populated.
    const base = $recordingStatus.durationMs ?? 0;
    return base;
  });
</script>

{#if recording}
  <a class="recording" href="#/record" title={`Open recording — ${$recordingStatus.name ?? ''}`.trim()}>
    <span class="rec-dot" aria-hidden="true"></span>
    <span class="rec-label">RECORDING</span>
    <span class="rec-time">{fmtHhMm(elapsedMs)}</span>
  </a>
{:else}
  <span class="health {$health}">
    {label}{#if streaming}<span class="streaming"> · streaming</span>{/if}
  </span>
{/if}

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

  .recording {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.06em;
    color: var(--err);
    padding: 2px 8px;
    border-radius: 12px;
    border: 1px solid rgba(255,80,80,0.5);
    background: rgba(255,80,80,0.08);
    text-decoration: none;
    cursor: pointer;
  }
  .recording:hover {
    background: rgba(255,80,80,0.16);
    border-color: rgba(255,80,80,0.7);
  }
  .recording:focus-visible {
    outline: 2px solid var(--err);
    outline-offset: 2px;
  }
  .rec-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--err);
    box-shadow: 0 0 4px rgba(255,80,80,0.7);
    animation: rec-pulse 1.2s ease-in-out infinite;
    flex-shrink: 0;
  }
  .rec-time {
    color: var(--text);
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  @keyframes rec-pulse {
    0%, 100% { opacity: 0.55; transform: scale(0.9); }
    50%      { opacity: 1;    transform: scale(1.15); }
  }
</style>
