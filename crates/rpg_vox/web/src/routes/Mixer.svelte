<script>
  import { onMount, onDestroy } from 'svelte';
  import * as api from '../lib/api.js';
  import { settings } from '../lib/stores.js';

  // Mixer state mirror. Populated from GET /mixer on mount; every knob
  // change PUTs a partial patch and adopts the server's echoed snapshot.
  let mixer = $state(null);
  let status = $state({ text: '', kind: '' });
  let saving = $state(0); // in-flight count so the status can show 'saving…'

  // VU levels — one entry per strip plus master L/R. Fresh peak (from the
  // most recent poll) drives the bar height; peak-hold decays slowly so
  // brief transients stay visible.
  let level = $state({ tts: 0, music: 0, vox: 0, master_l: 0, master_r: 0 });
  let hold  = $state({ tts: 0, music: 0, vox: 0, master_l: 0, master_r: 0 });
  let overrun = $state({ tts: false, music: false, vox: false, master_l: false, master_r: false });

  // Poll cadence for /mixer/levels. 33 ms ≈ 30 Hz — smooth enough to read
  // as continuous motion without turning the endpoint into a hot loop.
  const LEVEL_POLL_MS = 33;
  // Peak-hold decay: fraction of the previous hold that survives each
  // poll. 0.94 at 30 Hz → half-life ~350 ms, matching hardware VUs.
  const HOLD_DECAY = 0.94;
  // When we haven't heard from the server for a while (tab backgrounded,
  // network hiccup), fall back to zero rather than freezing on stale data.
  const STALE_ZERO_AFTER_MS = 500;

  let levelTimer = null;
  let lastLevelAt = 0;

  // Input strips in board order (left-to-right). `key` matches the mixer
  // payload's field name so the patch construction is a plain `{ [key]: … }`.
  const nodeName = $derived($settings?.node_name || 'rpg-vox');
  const strips = $derived([
    { key: 'tts',   label: 'TTS',   sub: 'synth → mic' },
    { key: 'music', label: 'Music', sub: `${nodeName}-music` },
    { key: 'vox',   label: 'Vox',   sub: `${nodeName}-vox` },
  ]);

  const MAX_GAIN = 2.0; // matches mixer::MAX_GAIN on the server
  // VU meter dB range. Bottom of the bar is `METER_MIN_DB`, top (100%) is
  // 0 dBFS. Anything above 0 dBFS lights the clip indicator.
  const METER_MIN_DB = -60;

  onMount(async () => {
    try {
      mixer = await api.getMixer();
    } catch (e) {
      status = { text: `load error: ${e.message}`, kind: 'err' };
    }
    // Start polling regardless of whether the initial fetch succeeded — the
    // /mixer/levels endpoint is independent of the settings snapshot and
    // will start reporting audio the moment it flows.
    startLevelPoll();
  });

  onDestroy(() => stopLevelPoll());

  function startLevelPoll() {
    if (levelTimer) return;
    const tick = async () => {
      try {
        const l = await api.getMixerLevels();
        lastLevelAt = performance.now();
        applyLevels(l);
      } catch {
        // Silent — /mixer/levels is a background poll, we don't want to
        // pop errors for a single dropped tick. If the server is truly
        // gone the health dot in the menubar will already reflect it.
        if (performance.now() - lastLevelAt > STALE_ZERO_AFTER_MS) {
          level = { tts: 0, music: 0, vox: 0, master_l: 0, master_r: 0 };
        }
      }
    };
    tick();
    levelTimer = setInterval(tick, LEVEL_POLL_MS);
  }
  function stopLevelPoll() {
    if (levelTimer) { clearInterval(levelTimer); levelTimer = null; }
  }

  function applyLevels(l) {
    // Server returns per-strip peaks in ~[0, 1] (may slightly exceed 1.0
    // when strip pans + gains sum before the master clamp — surface as
    // a clip indicator).
    const nextLevel = {
      tts: l.tts ?? 0,
      music: l.music ?? 0,
      vox: l.vox ?? 0,
      master_l: l.master_l ?? 0,
      master_r: l.master_r ?? 0,
    };
    const nextHold = {};
    const nextOver = {};
    for (const k of Object.keys(nextLevel)) {
      const v = nextLevel[k];
      nextHold[k] = Math.max(v, hold[k] * HOLD_DECAY);
      nextOver[k] = v >= 0.999;
    }
    level = nextLevel;
    hold = nextHold;
    overrun = nextOver;
  }

  function gainToDb(g) {
    if (g <= 0) return -Infinity;
    return 20 * Math.log10(g);
  }
  function fmtDb(g) {
    const db = gainToDb(g);
    if (!Number.isFinite(db)) return '-∞ dB';
    if (Math.abs(db) < 0.05) return '0.0 dB';
    return `${db > 0 ? '+' : ''}${db.toFixed(1)} dB`;
  }
  function fmtPan(p) {
    if (Math.abs(p) < 0.005) return 'C';
    const pct = Math.round(Math.abs(p) * 100);
    return `${p < 0 ? 'L' : 'R'}${pct}`;
  }

  /// Map a linear magnitude in [0, 1+] to a 0..1 bar-fill height using a
  /// dB scale (METER_MIN_DB → 0, 0 dBFS → 1). Anything above 0 dBFS is
  /// visually clamped at the top but signalled by the clip indicator.
  function meterFillPct(mag) {
    if (mag <= 0) return 0;
    const db = 20 * Math.log10(Math.min(mag, 1));
    return Math.max(0, Math.min(1, 1 + db / -METER_MIN_DB)) * 100;
  }

  // Persist a patch and adopt the returned snapshot so the client's view
  // always matches what the pw thread is actually reading. Silent on
  // success (the knob movement is its own feedback); errors surface in
  // the status strip.
  async function push(patch) {
    saving++;
    try {
      const next = await api.updateMixer(patch);
      mixer = next;
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `save failed: ${e.message}`, kind: 'err' };
    } finally {
      saving--;
    }
  }

  function updateStrip(key, field, value) {
    if (!mixer) return;
    // Optimistic local update so the knob follows the pointer without
    // waiting on the round-trip. The push() call below will authoritative-
    // overwrite `mixer` with the server snapshot when the reply lands.
    mixer[key] = { ...mixer[key], [field]: value };
    push({ [key]: { [field]: value } });
  }
  function updateMaster(field, value) {
    if (!mixer) return;
    mixer.master = { ...mixer.master, [field]: value };
    push({ master: { [field]: value } });
  }

  function toggleStripMute(key) {
    updateStrip(key, 'mute', !mixer[key].mute);
  }
  function toggleMasterMute() {
    updateMaster('mute', !mixer.master.mute);
  }
  function resetStripPan(key) {
    updateStrip(key, 'pan', 0);
  }
  function resetStripGain(key) {
    updateStrip(key, 'gain', 1);
  }
  function resetMasterGain() {
    updateMaster('gain', 1);
  }
</script>

<div class="mixer">
  <div class="header">
    <h1>Mixer</h1>
    <div class="hint">
      double-click a fader or pan slider to reset · click 0 dB to unity
      {#if saving > 0} · saving…{/if}
    </div>
  </div>

  {#if !mixer}
    <div class="empty">loading…</div>
  {:else}
    <div class="board">
      {#each strips as strip (strip.key)}
        <div class="strip" class:muted={mixer[strip.key].mute}>
          <div class="strip-label">
            <div class="name">{strip.label}</div>
            <div class="sub">{strip.sub}</div>
          </div>

          <div class="pan-row">
            <span class="pan-txt">{fmtPan(mixer[strip.key].pan)}</span>
            <input
              type="range"
              class="pan"
              min="-1" max="1" step="0.01"
              value={mixer[strip.key].pan}
              oninput={(e) => updateStrip(strip.key, 'pan', Number(e.currentTarget.value))}
              ondblclick={() => resetStripPan(strip.key)}
              aria-label="{strip.label} pan">
            <div class="pan-scale"><span>L</span><span>C</span><span>R</span></div>
          </div>

          <div class="fader-row">
            <div class="fader-scale">
              <span>+6</span><span>0</span><span>-12</span><span>-∞</span>
            </div>
            <input
              type="range"
              class="fader"
              min="0" max={MAX_GAIN} step="0.01"
              value={mixer[strip.key].gain}
              oninput={(e) => updateStrip(strip.key, 'gain', Number(e.currentTarget.value))}
              ondblclick={() => resetStripGain(strip.key)}
              aria-label="{strip.label} gain">
            <div class="vu" class:clip={overrun[strip.key]} aria-hidden="true">
              <div class="vu-fill" style="height: {meterFillPct(level[strip.key])}%"></div>
              <div class="vu-hold" style="bottom: {meterFillPct(hold[strip.key])}%"></div>
            </div>
          </div>

          <div class="gain-txt" title="click to set 0 dB">
            <button type="button" class="db-btn" onclick={() => resetStripGain(strip.key)}>
              {fmtDb(mixer[strip.key].gain)}
            </button>
          </div>

          <button
            type="button"
            class="mute"
            class:on={mixer[strip.key].mute}
            onclick={() => toggleStripMute(strip.key)}
            aria-pressed={mixer[strip.key].mute}
          >MUTE</button>
        </div>
      {/each}

      <div class="strip master" class:muted={mixer.master.mute}>
        <div class="strip-label">
          <div class="name">Master</div>
          <div class="sub">mic feed</div>
        </div>
        <!-- Master has no pan (it controls the sum L+R); the placeholder
             keeps every strip the same height so faders line up. -->
        <div class="pan-row placeholder"></div>

        <div class="fader-row master-fader">
          <div class="fader-scale">
            <span>+6</span><span>0</span><span>-12</span><span>-∞</span>
          </div>
          <input
            type="range"
            class="fader"
            min="0" max={MAX_GAIN} step="0.01"
            value={mixer.master.gain}
            oninput={(e) => updateMaster('gain', Number(e.currentTarget.value))}
            ondblclick={resetMasterGain}
            aria-label="Master gain">
          <!-- Stereo pair: L on the left, R on the right of the fader. -->
          <div class="vu" class:clip={overrun.master_l} aria-hidden="true">
            <div class="vu-fill" style="height: {meterFillPct(level.master_l)}%"></div>
            <div class="vu-hold" style="bottom: {meterFillPct(hold.master_l)}%"></div>
            <div class="vu-tag">L</div>
          </div>
          <div class="vu" class:clip={overrun.master_r} aria-hidden="true">
            <div class="vu-fill" style="height: {meterFillPct(level.master_r)}%"></div>
            <div class="vu-hold" style="bottom: {meterFillPct(hold.master_r)}%"></div>
            <div class="vu-tag">R</div>
          </div>
        </div>

        <div class="gain-txt" title="click to set 0 dB">
          <button type="button" class="db-btn" onclick={resetMasterGain}>
            {fmtDb(mixer.master.gain)}
          </button>
        </div>

        <button
          type="button"
          class="mute"
          class:on={mixer.master.mute}
          onclick={toggleMasterMute}
          aria-pressed={mixer.master.mute}
        >MUTE</button>
      </div>
    </div>
  {/if}

  <div class="status {status.kind}">{status.text}</div>
</div>

<style>
  .mixer { display: flex; flex-direction: column; gap: 16px; padding: 20px; max-width: 900px; margin: 0 auto; }
  .header { display: flex; justify-content: space-between; align-items: baseline; gap: 12px; }
  h1 { font-size: 20px; margin: 0; }
  .hint { color: var(--muted); font-size: 12px; }

  .board {
    display: grid;
    grid-template-columns: repeat(3, minmax(130px, 1fr)) minmax(170px, 1.3fr);
    gap: 14px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 18px;
  }

  .strip {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 14px 10px;
    border-radius: 8px;
    background: rgba(0,0,0,0.15);
    border: 1px solid var(--border);
    transition: opacity 0.15s;
  }
  .strip.master {
    background: rgba(122,162,255,0.05);
    border-color: rgba(122,162,255,0.25);
  }
  .strip.muted { opacity: 0.55; }

  .strip-label { text-align: center; }
  .strip-label .name { font-weight: 700; font-size: 14px; }
  .strip-label .sub  { color: var(--muted); font-size: 11px; }

  .pan-row {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    width: 100%;
  }
  .pan-row.placeholder { visibility: hidden; height: 48px; }
  .pan-txt {
    font-size: 11px;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .pan {
    width: 90%;
    margin: 0;
  }
  .pan-scale {
    display: flex;
    justify-content: space-between;
    width: 90%;
    color: var(--muted);
    font-size: 10px;
  }

  .fader-row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 8px;
    align-items: stretch;
    height: 200px;
    width: 100%;
    justify-content: center;
  }
  .fader-row.master-fader {
    /* Master gets two meters (L/R) flanking the fader. */
    grid-template-columns: auto 1fr auto auto;
  }
  .fader-scale {
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    color: var(--muted);
    font-size: 10px;
    padding: 6px 0;
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .fader {
    -webkit-appearance: slider-vertical;
    appearance: slider-vertical;
    writing-mode: vertical-lr;
    direction: rtl;
    width: 24px;
    height: 100%;
    margin: 0 auto;
    background: transparent;
  }

  /* VU meter: vertical bar with a dB-scaled fill and a peak-hold tick.
     Background is a static gradient (dark green → green → yellow → red)
     revealed by the fill's height. */
  .vu {
    position: relative;
    width: 10px;
    height: 100%;
    border-radius: 3px;
    background: #0b0f14;
    border: 1px solid var(--border);
    overflow: hidden;
    flex-shrink: 0;
  }
  .vu-fill {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    /* dark green (bottom) → green → yellow (~ -12 dB) → red (top).
       -60 dBFS → 0%, 0 dBFS → 100% — see meterFillPct(). */
    background: linear-gradient(to top,
      #0f4c1c 0%,
      #2ecc4a 55%,
      #f2c94c 80%,
      #ff5c5c 100%);
    transition: height 40ms linear;
  }
  .vu-hold {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    background: rgba(255,255,255,0.85);
    box-shadow: 0 0 3px rgba(255,255,255,0.5);
    transition: bottom 80ms linear;
    pointer-events: none;
  }
  .vu.clip { box-shadow: 0 0 0 1px var(--err), 0 0 8px rgba(255,80,80,0.6); }
  .vu-tag {
    position: absolute;
    bottom: -14px;
    left: 50%;
    transform: translateX(-50%);
    color: var(--muted);
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.04em;
  }

  .gain-txt {
    font-variant-numeric: tabular-nums;
  }
  .db-btn {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 3px 8px;
    font-size: 11px;
    font-weight: 500;
    border-radius: 4px;
    min-width: 60px;
    cursor: pointer;
  }
  .db-btn:hover { background: rgba(255,255,255,0.05); }

  .mute {
    padding: 5px 14px;
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-weight: 700;
    font-size: 11px;
    letter-spacing: 0.08em;
    cursor: pointer;
  }
  .mute:hover { background: rgba(255,255,255,0.05); color: var(--text); }
  .mute.on {
    background: var(--err);
    color: #0b0d10;
    border-color: var(--err);
  }
</style>
