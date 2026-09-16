<script>
  import { health, recordingStatus, pingStats, pingBad } from '../lib/stores.js';
  import { monitorState, monitorPref, monitorStats } from '../lib/browserMonitor.js';
  import { navigate } from '../lib/router.js';

  const label = $derived({ checking: 'checking…', ok: 'online', err: 'offline' }[$health] || '');
  // Only decorate with "streaming" when the app is healthy AND the browser
  // monitor is actively receiving (WS open, decoder configured). Any other
  // monitor state ('connecting', 'error', 'stopped') is quiet — no need to
  // clutter the menubar with transient states.
  const streaming = $derived($health === 'ok' && $monitorState === 'listening');
  // Auto-paused because the ping meter tripped. Shown to the user as a
  // yellow "LAGGING Xms" so they know why the browser monitor went silent
  // — the pref is still on and we'll auto-resume when the link recovers.
  const lagging = $derived(
    $monitorPref && ($monitorState === 'latency-paused' || ($pingBad && $health === 'ok'))
  );
  // Tooltip text for the "streaming" decorator — surfaces the AudioWorklet's
  // live buffer depth (with peak + target for context) and the current ping
  // average so hovering shows both the audio queue and the network round-trip
  // that drives auto-pause.
  const streamingTitle = $derived.by(() => {
    const buf = `buffer ${Math.round($monitorStats.depthMs)} ms `
      + `(peak ${Math.round($monitorStats.peakMs)} ms, target ${Math.round($monitorStats.targetMs)} ms)`;
    let ping;
    if (!$pingStats.sampled) ping = 'ping —';
    else if ($pingStats.err) ping = 'ping no response';
    else ping = `ping ${Math.round($pingStats.lastMs)} ms (avg ${Math.round($pingStats.avgMs)} ms)`;
    return `streaming — ${buf} · ${ping}`;
  });

  // Flash a "copied" pill for ~900 ms after a successful clipboard write, so
  // the click has visible feedback without a heavier toast system.
  let copiedFlash = $state(false);
  let copiedTimer = null;

  async function onStreamingClick() {
    try {
      await navigator.clipboard?.writeText(streamingTitle);
      copiedFlash = true;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => { copiedFlash = false; }, 900);
    } catch {}
    navigate('/mixer');
    // Mixer mounts asynchronously after the hashchange, and continues to
    // reflow as sinks/sources/PipeWire graph render — the anchor's absolute
    // page position keeps drifting for hundreds of ms. Poll rAF: re-scroll
    // whenever the anchor's page-space top has moved, and only stop once
    // it's held steady for a run of frames (or the settle budget elapses).
    // `behavior: 'auto'` (instant) avoids fighting an in-flight smooth
    // animation while we're chasing a moving target.
    const SETTLE_MS = 2500;
    const STABLE_FRAMES = 10;
    const startedAt = performance.now();
    let lastPageY = null;
    let stable = 0;
    const tick = () => {
      const el = document.getElementById('web-monitor');
      const nowMs = performance.now();
      if (!el) {
        if (nowMs - startedAt < SETTLE_MS) requestAnimationFrame(tick);
        return;
      }
      const pageY = el.getBoundingClientRect().top + window.scrollY;
      if (lastPageY !== null && Math.abs(pageY - lastPageY) < 0.5) {
        stable++;
      } else {
        stable = 0;
        el.scrollIntoView({ block: 'start', behavior: 'auto' });
      }
      lastPageY = pageY;
      if (stable < STABLE_FRAMES && nowMs - startedAt < SETTLE_MS) {
        requestAnimationFrame(tick);
      }
    };
    requestAnimationFrame(tick);
  }
  const laggingMs = $derived(
    $pingStats.sampled && !$pingStats.err ? Math.round($pingStats.avgMs) : null
  );

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
    {label}{#if lagging}<span class="lagging" title="Web Monitor auto-paused — waiting for ping to recover"> · LAGGING{#if laggingMs != null} {laggingMs}ms{:else if $pingStats.err} · no response{/if}</span>{:else if streaming}<button type="button" class="streaming" title={streamingTitle} onclick={onStreamingClick}> · {copiedFlash ? 'copied stats' : 'streaming'}</button>{/if}
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
    background: transparent;
    border: none;
    padding: 0;
    margin: 0;
    font: inherit;
    cursor: pointer;
  }
  .streaming:hover { text-decoration: underline; }
  .streaming:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: 2px;
  }
  /* Auto-paused because ping crossed the threshold. Yellow so it reads as
     a warning (not an error, since the pref is still on and we'll come
     back automatically) and tabular-nums keeps the ms figure from jittering
     the header width as the value updates every ~2 s. */
  .lagging {
    color: #e0b400;
    font-weight: 600;
    letter-spacing: 0.03em;
    font-variant-numeric: tabular-nums;
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
