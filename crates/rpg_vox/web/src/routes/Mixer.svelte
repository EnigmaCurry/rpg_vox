<script>
  import { onMount, onDestroy } from 'svelte';
  import * as api from '../lib/api.js';
  import {
    settings,
    graph,
    reloadGraph,
    getPwMonitorPref,
    setPwMonitorPref,
    getClientId,
  } from '../lib/stores.js';
  import * as browserMonitor from '../lib/browserMonitor.js';
  import { monitorState, monitorStats, monitorPref } from '../lib/browserMonitor.js';
  import * as browserMic from '../lib/browserMic.js';
  import { micState, micPref, micDevice, micMode } from '../lib/browserMic.js';
  import { audioAncillary } from '../lib/audioClaim.js';
  import {
    pingStats,
    pingBad,
    PING_DISABLE_MS,
    PING_BAD_STREAK,
    PING_INTERVAL_MS,
  } from '../lib/stores.js';

  // Stable per-browser client id used by the web-mic pool. Read once
  // on mount and reused for the WS + every routing call; never changes
  // for the lifetime of this tab (localStorage).
  const clientId = getClientId();
  const browserMicSupported = browserMic.isSupported();
  // Enumerated audio-input devices, populated after mount. Labels may
  // be empty until the user grants mic permission once — we still
  // include devices with empty labels so the dropdown isn't stuck
  // saying "no devices" before the first grant.
  let inputDevices = $state([]);
  let webMicError = $state('');
  let webMicBusy = $state(false);

  // Mixer state mirror. Populated from GET /mixer on mount; every knob
  // change PUTs a partial patch and adopts the server's echoed snapshot.
  //   { tts, music, vox: [{enabled, to_output, name, channel: {gain, pan, mute}}, ...], master }
  let mixer = $state(null);
  let status = $state({ text: '', kind: '' });
  let saving = $state(0); // in-flight count so the status can show 'saving…'

  // External Stream/Output/Audio producers visible in the pw graph
  // (browser tabs, media players, etc.), and the pipewire sinks the user
  // can pin the local monitor to. Both come from the shared `graph` store,
  // refreshed on a slow cadence.
  const sources = $derived($graph?.sources ?? []);
  let graphTimer = null;
  const GRAPH_POLL_MS = 2000;

  // Local monitor state — mirrors the Settings-page controls that used to
  // live under /settings. Persisted to localStorage via getPwMonitorPref /
  // setPwMonitorPref; App.svelte re-applies the pref at boot.
  let monitorRestored = false;
  let monitorStatus = $state({ text: '', kind: '' });
  let browserMonitorBusy = $state(false);
  let browserMonitorError = $state('');
  const browserMonitorSupported = browserMonitor.isSupported();
  // "Latency paused" = the monitor is auto-paused by the global ping
  // subscription in browserMonitor.js. Derived here for message text; the
  // pref (user intent) stays on throughout, so the checkbox stays checked.
  const latencyPaused = $derived($monitorState === 'latency-paused' || ($monitorPref && $pingBad));

  // Sinks the user might reasonably want to monitor rpg_vox with. Excludes:
  //   * the auto-patch target (we're already routing to it)
  //   * our own companion sinks (`{node_name}-music`, `{node_name}-vox`)
  function monitorableFrom(g, ownPrefix) {
    const skip = ownPrefix ? `${ownPrefix}-` : null;
    return g.sinks.filter((s) =>
      s.id !== g.auto_patch_sink_id
      && s.name !== g.auto_patch_target
      && (!skip || !s.name.startsWith(skip))
    );
  }
  const ownNodePrefix = $derived($settings?.node_name || '');
  const monitorable = $derived($graph ? monitorableFrom($graph, ownNodePrefix) : []);

  // VU levels. tts/music/master_l/master_r are scalars; vox is an array
  // (one entry per slot) so the levels shape mirrors the mixer state.
  let level = $state({ tts: 0, music: 0, vox: [], master_l: 0, master_r: 0 });
  let hold  = $state({ tts: 0, music: 0, vox: [], master_l: 0, master_r: 0 });
  let overrun = $state({ tts: false, music: false, vox: [], master_l: false, master_r: false });

  const LEVEL_POLL_MS = 33;
  const HOLD_DECAY = 0.94;
  const STALE_ZERO_AFTER_MS = 500;

  let levelTimer = null;
  let lastLevelAt = 0;

  const nodeName = $derived($settings?.node_name || 'rpg-vox');
  const voxCount = $derived(mixer?.vox?.length ?? 0);
  // Per-slot rename state — we key by slot index so multiple slot
  // labels could in principle be edited at once, even though the UI
  // only surfaces one at a time.
  let renamingSlot = $state(-1);
  let renameText = $state('');

  const MAX_GAIN = 2.0;
  const METER_MIN_DB = -60;

  // PipeWire node suffix for slot i: slot 0 is `-vox` (back-compat),
  // slot i>=1 is `-vox{i+1}`. Matches SinkRole::suffix() on the server.
  function slotSuffix(i) {
    return i === 0 ? 'vox' : `vox${i + 1}`;
  }

  onMount(async () => {
    try {
      mixer = await api.getMixer();
    } catch (e) {
      status = { text: `load error: ${e.message}`, kind: 'err' };
    }
    startLevelPoll();
    refreshGraph();
    graphTimer = setInterval(refreshGraph, GRAPH_POLL_MS);
    // Restore the Web microphone input pref: if the user had it on
    // last session, open the presence WS now so the source row shows
    // up in Sources immediately. Capture still requires a user
    // gesture (a destination pick in Sources) so we don't call
    // getUserMedia here.
    if (browserMicSupported && $micPref) {
      try {
        await browserMic.startPresence();
        inputDevices = await browserMic.listInputDevices();
      } catch (e) {
        webMicError = e?.message || 'web mic auto-restore failed';
      }
    } else if (browserMicSupported) {
      inputDevices = await browserMic.listInputDevices();
    }
  });

  onDestroy(() => {
    stopLevelPoll();
    if (graphTimer) { clearInterval(graphTimer); graphTimer = null; }
  });

  async function refreshGraph() {
    try { await reloadGraph(); } catch {}
  }

  // Fallback restore: App.svelte kicks off the same restore at boot, but if
  // the saved sink wasn't in the graph by then this effect will pick it up
  // once it appears (e.g. the user plugged in headphones after launch).
  $effect(() => {
    const g = $graph;
    if (!g || monitorRestored || g.monitor_sink_id != null) return;
    const wanted = getPwMonitorPref();
    if (!wanted) return;
    const match = monitorableFrom(g, ownNodePrefix).find((s) => s.name === wanted);
    if (match) {
      monitorRestored = true;
      startMonitor(match.id, { restore: true });
    }
  });

  // Web-mic gesture-triggered auto-resume is armed at the App level
  // (see App.svelte) so it works from any route — reloading on
  // /script with a persisted mic routing must still resume on the
  // first user click without requiring a visit to /mixer.

  async function startMonitor(sinkId, opts = {}) {
    monitorRestored = true;
    const { ok, body } = await api.startMonitor(sinkId);
    if (!ok) {
      monitorStatus = { text: `monitor error: ${body?.error || 'unknown'}`, kind: 'err' };
    } else {
      const sink = ($graph?.sinks || []).find((s) => s.id === sinkId);
      if (sink?.name) setPwMonitorPref(sink.name);
      monitorStatus = {
        text: opts.restore ? `restored monitor: sink #${sinkId}` : `monitoring sink #${sinkId}`,
        kind: 'ok',
      };
    }
    await reloadGraph();
  }

  async function stopMonitor() {
    monitorRestored = true;
    const { ok, body } = await api.stopMonitor();
    if (!ok) {
      monitorStatus = { text: `stop error: ${body?.error || 'unknown'}`, kind: 'err' };
    } else {
      setPwMonitorPref(null);
      monitorStatus = { text: 'monitor stopped', kind: 'ok' };
    }
    await reloadGraph();
  }

  async function toggleBrowserMonitor(ev) {
    const wantOn = ev.currentTarget.checked;
    browserMonitorBusy = true;
    browserMonitorError = '';
    try {
      if (wantOn) {
        browserMonitor.persistPref(true);
        if ($pingBad) {
          // Pref is now on; the global pingBad subscription will fire
          // start() when latency recovers. Don't start now — the buffer
          // can't hide the current RTT.
          monitorStatus = { text: 'browser monitor queued — waiting for latency to recover', kind: 'ok' };
        } else {
          await browserMonitor.start((s) => {
            if (s.state === 'error') {
              browserMonitorError = s.detail || 'monitor error';
            }
          });
          monitorStatus = { text: 'browser monitor on', kind: 'ok' };
        }
      } else {
        browserMonitor.persistPref(false);
        await browserMonitor.stop();
        monitorStatus = { text: 'browser monitor off', kind: 'ok' };
      }
    } catch (e) {
      // Keep the pref set — user intent survives a transient start
      // failure. The pingBad subscribe / next reload will retry.
      browserMonitorError = e?.message || 'monitor failed';
      monitorStatus = { text: `browser monitor: ${browserMonitorError}`, kind: 'err' };
    } finally {
      browserMonitorBusy = false;
    }
  }

  async function routeSource(id, target) {
    try {
      await api.linkSource(id, target);
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `route failed: ${e.message}`, kind: 'err' };
    }
    await refreshGraph();
  }

  async function unrouteSource(id) {
    try {
      await api.unlinkSource(id);
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `unroute failed: ${e.message}`, kind: 'err' };
    }
    await refreshGraph();
  }

  // Web-mic rows appear alongside pipewire sources but are keyed by
  // client_uuid rather than pipewire node id, so route calls use the
  // web-mic endpoint. Clicking a destination on our OWN row also
  // starts local capture in the primary tab (getUserMedia — the
  // click serves as the user gesture that unlocks it); Off stops
  // capture. In an ancillary tab (another tab in this browser is
  // already holding audio), we're just controlling the primary
  // tab's routing server-side and never touch local mic hardware.
  async function routeWebMic(uuid, target) {
    const isOwn = uuid === clientId;
    const runsLocalCapture = isOwn && !$audioAncillary;
    try {
      if (runsLocalCapture) {
        // Acquire the mic BEFORE telling the server — otherwise the
        // slot's routing atomic flips on but the WS carries zero PCM
        // for a beat, which is confusing to hear on the receiving end.
        await browserMic.startCapture();
      }
      await api.routeWebMic(uuid, target);
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `route failed: ${e.message}`, kind: 'err' };
    }
    await refreshGraph();
  }

  async function unrouteWebMic(uuid) {
    const isOwn = uuid === clientId;
    const runsLocalCapture = isOwn && !$audioAncillary;
    try {
      await api.unrouteWebMic(uuid);
      if (runsLocalCapture) {
        // Halt the mic hardware for the current tab. Presence WS
        // stays open so the row remains visible with Off selected.
        await browserMic.stopCapture();
      }
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `unroute failed: ${e.message}`, kind: 'err' };
    }
    await refreshGraph();
  }

  // Merge pipewire sources + web-mic sources into one list rendered by
  // the Sources block. Each web-mic entry is normalized to look like
  // a SourceInfo (id/kind/labels) plus a `client_uuid` marker so we
  // can style our own row and route through the correct endpoint.
  const mergedSources = $derived([
    ...sources.map((s) => ({ ...s, srcKind: 'pw' })),
    ...(($graph?.web_mic_sources ?? []).map((w) => ({
      srcKind: 'web_mic',
      id: `wm:${w.client_uuid}`,
      client_uuid: w.client_uuid,
      kind: 'web_mic',
      name: 'web microphone',
      description: 'Web microphone',
      application_name: 'Web browser',
      media_name: null,
      pid: null,
      icon_name: null,
      routed_to: w.routed_to,
      isOwn: w.client_uuid === clientId,
    }))),
  ]);

  async function toggleWebMic(ev) {
    const wantOn = ev.currentTarget.checked;
    webMicBusy = true;
    webMicError = '';
    try {
      if (wantOn) {
        browserMic.persistPref(true);
        await browserMic.startPresence();
        // Refresh the device list once presence is open — some
        // browsers only fill in device labels after the page has
        // held a mic permission at least once.
        inputDevices = await browserMic.listInputDevices();
      } else {
        browserMic.persistPref(false);
        await browserMic.stopPresence();
      }
    } catch (e) {
      webMicError = e?.message || 'web mic failed';
    } finally {
      webMicBusy = false;
    }
    await refreshGraph();
  }

  function onDeviceChange(ev) {
    const id = ev.currentTarget.value || null;
    browserMic.persistDevice(id);
    // If we're actively capturing, restart with the new device so the
    // switch takes effect immediately. Otherwise the next startCapture
    // will pick it up naturally.
    if ($micState.capture === 'active') {
      browserMic.stopCapture().then(() => browserMic.startCapture()).catch(() => {});
    }
  }

  function onMuteModeChange(mode) {
    browserMic.persistMode(mode);
  }

  function sourceLabel(s) {
    if (s.kind === 'device') return s.description || s.name || `#${s.id}`;
    return s.media_name || s.description || s.name || `#${s.id}`;
  }
  function sourceApp(s) {
    if (s.kind === 'device') return 'input device';
    return s.application_name || s.name || 'audio';
  }

  function startLevelPoll() {
    if (levelTimer) return;
    const tick = async () => {
      try {
        const l = await api.getMixerLevels();
        lastLevelAt = performance.now();
        applyLevels(l);
      } catch {
        if (performance.now() - lastLevelAt > STALE_ZERO_AFTER_MS) {
          level = { tts: 0, music: 0, vox: new Array(voxCount).fill(0), master_l: 0, master_r: 0 };
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
    const voxArr = Array.isArray(l.vox) ? l.vox : [];
    const nextLevel = {
      tts: l.tts ?? 0,
      music: l.music ?? 0,
      vox: voxArr.map((v) => v ?? 0),
      master_l: l.master_l ?? 0,
      master_r: l.master_r ?? 0,
    };
    // Pad hold/overrun to at least voxArr.length to avoid undefined reads.
    while (hold.vox.length < voxArr.length) hold.vox.push(0);
    while (overrun.vox.length < voxArr.length) overrun.vox.push(false);
    const nextHold = {
      tts: Math.max(nextLevel.tts, (hold.tts ?? 0) * HOLD_DECAY),
      music: Math.max(nextLevel.music, (hold.music ?? 0) * HOLD_DECAY),
      vox: nextLevel.vox.map((v, i) => Math.max(v, (hold.vox[i] ?? 0) * HOLD_DECAY)),
      master_l: Math.max(nextLevel.master_l, (hold.master_l ?? 0) * HOLD_DECAY),
      master_r: Math.max(nextLevel.master_r, (hold.master_r ?? 0) * HOLD_DECAY),
    };
    const nextOver = {
      tts: nextLevel.tts >= 0.999,
      music: nextLevel.music >= 0.999,
      vox: nextLevel.vox.map((v) => v >= 0.999),
      master_l: nextLevel.master_l >= 0.999,
      master_r: nextLevel.master_r >= 0.999,
    };
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

  function meterFillPct(mag) {
    if (mag <= 0) return 0;
    const db = 20 * Math.log10(Math.min(mag, 1));
    return Math.max(0, Math.min(1, 1 + db / -METER_MIN_DB)) * 100;
  }

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

  // Strip = tts or music (the simple fixed strips). Vox strips route
  // through updateVoxSlot below.
  function updateStrip(key, field, value) {
    if (!mixer) return;
    mixer[key] = { ...mixer[key], [field]: value };
    push({ [key]: { [field]: value } });
  }
  function updateMaster(field, value) {
    if (!mixer) return;
    mixer.master = { ...mixer.master, [field]: value };
    push({ master: { [field]: value } });
  }
  function toggleStripMute(key) { updateStrip(key, 'mute', !mixer[key].mute); }
  function toggleMasterMute() { updateMaster('mute', !mixer.master.mute); }
  function resetStripPan(key) { updateStrip(key, 'pan', 0); }
  function resetStripGain(key) { updateStrip(key, 'gain', 1); }
  function resetMasterGain() { updateMaster('gain', 1); }

  // Vox slot updates. Server accepts `{ vox: { "<slot>": { channel:{gain}
  // } } }` for channel-level fields (gain/pan/mute), or `{ vox: { "<slot>":
  // { name?, enabled?, toOutput? } }` for slot-level metadata. The
  // patch schema flattens channel fields into the slot patch — see the
  // Rust VoxSlotPatch — so `{gain}`/`{pan}`/`{mute}` at the top level
  // patch through to the strip's ChannelPatch.
  function updateVoxChannel(slot, field, value) {
    if (!mixer?.vox?.[slot]) return;
    mixer.vox[slot].channel = { ...mixer.vox[slot].channel, [field]: value };
    push({ vox: { [String(slot)]: { [field]: value } } });
  }
  function updateVoxSlotMeta(slot, key, value) {
    if (!mixer?.vox?.[slot]) return;
    // toOutput on the wire uses camelCase per VoxSlotPatch's serde alias.
    mixer.vox[slot] = { ...mixer.vox[slot], [key]: value };
    // Server accepts either `to_output` or `toOutput` — send camelCase.
    const wireKey = key === 'to_output' ? 'toOutput' : key;
    push({ vox: { [String(slot)]: { [wireKey]: value } } });
  }
  function toggleVoxMute(slot) {
    updateVoxChannel(slot, 'mute', !mixer.vox[slot].channel.mute);
  }
  function toggleVoxEnabled(slot) {
    updateVoxSlotMeta(slot, 'enabled', !mixer.vox[slot].enabled);
  }
  function toggleVoxToOutput(slot) {
    updateVoxSlotMeta(slot, 'to_output', !mixer.vox[slot].to_output);
  }
  function resetVoxPan(slot) { updateVoxChannel(slot, 'pan', 0); }
  function resetVoxGain(slot) { updateVoxChannel(slot, 'gain', 1); }

  function startRename(slot) {
    renameText = mixer.vox[slot].name;
    renamingSlot = slot;
  }
  async function saveRename() {
    const slot = renamingSlot;
    renamingSlot = -1;
    if (slot < 0 || !mixer?.vox?.[slot]) return;
    const name = renameText.trim();
    if (!name || name === mixer.vox[slot].name) return;
    mixer.vox[slot].name = name;
    push({ vox: { [String(slot)]: { name } } });
  }
  function cancelRename() { renamingSlot = -1; }
</script>

<div class="mixer">
  <div class="header">
    <h1>Mixer</h1>
    <div class="hint">
      double-click a fader or pan slider to reset · click 0 dB to unity
      · click a Vox strip name to rename
      {#if saving > 0} · saving…{/if}
    </div>
  </div>

  {#if !mixer}
    <div class="empty">loading…</div>
  {:else}
    <div class="board">
      <!-- Fixed TTS strip -->
      <div class="strip" class:muted={mixer.tts.mute}>
        <div class="strip-label">
          <div class="name">TTS</div>
          <div class="sub">synth → mic</div>
        </div>
        <div class="pan-row">
          <span class="pan-txt">{fmtPan(mixer.tts.pan)}</span>
          <input type="range" class="pan" min="-1" max="1" step="0.01"
            value={mixer.tts.pan}
            oninput={(e) => updateStrip('tts', 'pan', Number(e.currentTarget.value))}
            ondblclick={() => resetStripPan('tts')} aria-label="TTS pan" />
          <div class="pan-scale"><span>L</span><span>C</span><span>R</span></div>
        </div>
        <div class="fader-row">
          <div class="fader-scale"><span>+6</span><span>0</span><span>-12</span><span>-∞</span></div>
          <input type="range" class="fader" min="0" max={MAX_GAIN} step="0.01"
            value={mixer.tts.gain}
            oninput={(e) => updateStrip('tts', 'gain', Number(e.currentTarget.value))}
            ondblclick={() => resetStripGain('tts')} aria-label="TTS gain" />
          <div class="vu" class:clip={overrun.tts} aria-hidden="true">
            <div class="vu-fill" style="height: {meterFillPct(level.tts)}%"></div>
            <div class="vu-hold" style="bottom: {meterFillPct(hold.tts)}%"></div>
          </div>
        </div>
        <div class="gain-txt"><button type="button" class="db-btn" onclick={() => resetStripGain('tts')}>{fmtDb(mixer.tts.gain)}</button></div>
        <button type="button" class="mute" class:on={mixer.tts.mute} onclick={() => toggleStripMute('tts')} aria-pressed={mixer.tts.mute}>MUTE</button>
      </div>

      <!-- Fixed Music strip -->
      <div class="strip" class:muted={mixer.music.mute}>
        <div class="strip-label">
          <div class="name">Music</div>
          <div class="sub">{nodeName}-music</div>
        </div>
        <div class="pan-row">
          <span class="pan-txt">{fmtPan(mixer.music.pan)}</span>
          <input type="range" class="pan" min="-1" max="1" step="0.01"
            value={mixer.music.pan}
            oninput={(e) => updateStrip('music', 'pan', Number(e.currentTarget.value))}
            ondblclick={() => resetStripPan('music')} aria-label="Music pan" />
          <div class="pan-scale"><span>L</span><span>C</span><span>R</span></div>
        </div>
        <div class="fader-row">
          <div class="fader-scale"><span>+6</span><span>0</span><span>-12</span><span>-∞</span></div>
          <input type="range" class="fader" min="0" max={MAX_GAIN} step="0.01"
            value={mixer.music.gain}
            oninput={(e) => updateStrip('music', 'gain', Number(e.currentTarget.value))}
            ondblclick={() => resetStripGain('music')} aria-label="Music gain" />
          <div class="vu" class:clip={overrun.music} aria-hidden="true">
            <div class="vu-fill" style="height: {meterFillPct(level.music)}%"></div>
            <div class="vu-hold" style="bottom: {meterFillPct(hold.music)}%"></div>
          </div>
        </div>
        <div class="gain-txt"><button type="button" class="db-btn" onclick={() => resetStripGain('music')}>{fmtDb(mixer.music.gain)}</button></div>
        <button type="button" class="mute" class:on={mixer.music.mute} onclick={() => toggleStripMute('music')} aria-pressed={mixer.music.mute}>MUTE</button>
      </div>

      <!-- Dynamic Vox slots -->
      {#each mixer.vox as slot, i (i)}
        <div class="strip vox-strip" class:muted={slot.channel.mute} class:disabled={!slot.enabled}>
          <div class="strip-label">
            {#if renamingSlot === i}
              <input class="rename-input" type="text"
                bind:value={renameText}
                onkeydown={(e) => {
                  if (e.key === 'Enter') { e.preventDefault(); saveRename(); }
                  else if (e.key === 'Escape') { e.preventDefault(); cancelRename(); }
                }}
                onblur={saveRename}
                aria-label="Vox slot {i + 1} name" />
            {:else}
              <button type="button" class="name name-btn" title="click to rename" onclick={() => startRename(i)}>{slot.name}</button>
            {/if}
            <div class="sub">{nodeName}-{slotSuffix(i)} · capture</div>
          </div>

          <div class="pan-row">
            <span class="pan-txt">{fmtPan(slot.channel.pan)}</span>
            <input type="range" class="pan" min="-1" max="1" step="0.01"
              value={slot.channel.pan} disabled={!slot.enabled}
              oninput={(e) => updateVoxChannel(i, 'pan', Number(e.currentTarget.value))}
              ondblclick={() => resetVoxPan(i)} aria-label="{slot.name} pan" />
            <div class="pan-scale"><span>L</span><span>C</span><span>R</span></div>
          </div>

          <div class="fader-row">
            <div class="fader-scale"><span>+6</span><span>0</span><span>-12</span><span>-∞</span></div>
            <input type="range" class="fader" min="0" max={MAX_GAIN} step="0.01"
              value={slot.channel.gain} disabled={!slot.enabled}
              oninput={(e) => updateVoxChannel(i, 'gain', Number(e.currentTarget.value))}
              ondblclick={() => resetVoxGain(i)} aria-label="{slot.name} gain" />
            <div class="vu" class:clip={overrun.vox[i]} aria-hidden="true">
              <div class="vu-fill" style="height: {meterFillPct(level.vox[i] ?? 0)}%"></div>
              <div class="vu-hold" style="bottom: {meterFillPct(hold.vox[i] ?? 0)}%"></div>
            </div>
          </div>

          <div class="gain-txt"><button type="button" class="db-btn" onclick={() => resetVoxGain(i)}>{fmtDb(slot.channel.gain)}</button></div>

          <button type="button" class="mute" class:on={slot.channel.mute}
            onclick={() => toggleVoxMute(i)} aria-pressed={slot.channel.mute}
            disabled={!slot.enabled}>MUTE</button>

          <button type="button" class="to-mic" class:on={slot.to_output}
            onclick={() => toggleVoxToOutput(i)} aria-pressed={slot.to_output}
            disabled={!slot.enabled}
            title={slot.to_output
              ? 'summing into the mic feed — click to keep capture-only'
              : 'capture-only (STT / recording tap) — click to also send to the mic'}
          >→ MIC</button>

          <button type="button" class="enable-btn" class:on={slot.enabled}
            onclick={() => toggleVoxEnabled(i)} aria-pressed={slot.enabled}
            title={slot.enabled ? 'disable this Vox channel' : 'enable this Vox channel'}
          >{slot.enabled ? 'ENABLED' : 'DISABLED'}</button>
        </div>
      {/each}

      <!-- Master -->
      <div class="strip master" class:muted={mixer.master.mute}>
        <div class="strip-label">
          <div class="name">Master</div>
          <div class="sub">mic feed</div>
        </div>
        <div class="pan-row placeholder"></div>
        <div class="fader-row master-fader">
          <div class="fader-scale"><span>+6</span><span>0</span><span>-12</span><span>-∞</span></div>
          <input type="range" class="fader" min="0" max={MAX_GAIN} step="0.01"
            value={mixer.master.gain}
            oninput={(e) => updateMaster('gain', Number(e.currentTarget.value))}
            ondblclick={resetMasterGain} aria-label="Master gain" />
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
        <div class="gain-txt"><button type="button" class="db-btn" onclick={resetMasterGain}>{fmtDb(mixer.master.gain)}</button></div>
        <button type="button" class="mute" class:on={mixer.master.mute} onclick={toggleMasterMute} aria-pressed={mixer.master.mute}>MUTE</button>
      </div>
    </div>
  {/if}

  <div class="sources">
    <div class="sources-header">
      <h2>Sources</h2>
      <div class="hint">
        route an audio-producing app (browser tab, media player…) into
        Music or a Vox slot. Default = leave the app's existing pipewire
        connection (usually your default output) untouched. Rows for
        web-microphone clients also appear here — your own is highlighted.
      </div>
    </div>
    {#if mergedSources.length === 0}
      <div class="empty">no external audio producers currently in the graph</div>
    {:else}
      <ul class="source-list">
        {#each mergedSources as s (s.id)}
          <li
            class="source-row"
            class:routed={s.routed_to}
            class:own-web-mic={s.srcKind === 'web_mic' && s.isOwn}
          >
            <div class="source-meta">
              <div class="source-app">
                {#if s.srcKind === 'web_mic'}
                  web microphone{#if s.isOwn} · this browser{/if}
                {:else}
                  {sourceApp(s)}
                {/if}
              </div>
              <div class="source-title" title={s.srcKind === 'web_mic' ? `client ${s.client_uuid}` : sourceLabel(s)}>
                {#if s.srcKind === 'web_mic'}
                  {s.isOwn ? 'Your microphone' : `client ${s.client_uuid.slice(0, 8)}…`}
                {:else}
                  {sourceLabel(s)}
                {/if}
              </div>
            </div>
            <div class="source-actions" role="radiogroup" aria-label="{s.srcKind === 'web_mic' ? 'web mic' : sourceLabel(s)} routing">
              <button type="button" class="route-btn"
                class:active={s.routed_to === 'music'}
                onclick={() => s.srcKind === 'web_mic' ? routeWebMic(s.client_uuid, 'music') : routeSource(s.id, 'music')}
                role="radio" aria-checked={s.routed_to === 'music'}>Music</button>
              {#each mixer?.vox ?? [] as slot, i (i)}
                {@const target = slotSuffix(i)}
                <button type="button" class="route-btn"
                  class:active={s.routed_to === target}
                  onclick={() => s.srcKind === 'web_mic' ? routeWebMic(s.client_uuid, target) : routeSource(s.id, target)}
                  role="radio" aria-checked={s.routed_to === target}>{slot.name}</button>
              {/each}
              <button type="button" class="route-btn"
                class:active={!s.routed_to}
                onclick={() => s.srcKind === 'web_mic' ? unrouteWebMic(s.client_uuid) : unrouteSource(s.id)}
                role="radio" aria-checked={!s.routed_to}
                title={
                  s.srcKind === 'web_mic'
                    ? (s.isOwn ? 'stop your microphone capture' : "don't feed this client's mic into rpg_vox")
                    : (s.kind === 'device'
                      ? 'do not feed this input into rpg_vox'
                      : "drop rpg_vox's routing link; app keeps its default connection")
                }
              >{(s.srcKind === 'web_mic' || s.kind === 'device') ? 'Off' : 'Default'}</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <section id="web-audio" class="io-panel" style="scroll-margin-top: 60px">
    <div class="subhead"><span>Web audio</span>
      <span class="note">this browser tab's audio-in and audio-out for rpg_vox</span>
    </div>
    {#if $audioAncillary}
      <!-- Another tab in this browser already holds web audio. Every
           control on the page still works, but the two audio streams
           (monitor + mic) stay off in this tab to avoid double-billing
           bandwidth and echoing the monitor twice. The primary tab
           dropping web audio flips this back to enabled automatically. -->
      <div class="monitor-err" role="status">
        Another browser tab is holding web audio for this session.
        Toggle off Web audio in that tab (or close it) to enable web
        monitor / microphone here.
      </div>
    {/if}
    <div class="sink-list">
      <label class="sink browser-monitor" title="Stream the tap into this browser tab (does not affect other clients)">
        <input
          type="checkbox"
          checked={$monitorPref}
          disabled={!browserMonitorSupported || browserMonitorBusy || $audioAncillary}
          onchange={toggleBrowserMonitor}>
        <span>Web audio monitor (master)</span>
        <span class="id">
          {#if !browserMonitorSupported}
            unsupported
          {:else if browserMonitorBusy}
            …
          {:else if latencyPaused}
            paused — high latency
          {:else if $monitorState === 'awaiting-gesture'}
            click to start
          {:else if $monitorState === 'listening'}
            listening
          {:else if $monitorState === 'connecting'}
            connecting…
          {:else if $monitorPref}
            starting…
          {:else}
            off
          {/if}
        </span>
      </label>
      {#if browserMonitorError}
        <div class="monitor-err">{browserMonitorError}</div>
      {/if}
      {#if latencyPaused}
        <div class="monitor-err">
          {#if $pingStats.err}
            no response from server — Web Monitor paused; will resume when the connection recovers
          {:else}
            sustained latency too high (avg {$pingStats.avgMs.toFixed(0)} ms &gt; {PING_DISABLE_MS} ms) — Web Monitor paused; will resume when the connection improves
          {/if}
        </div>
      {/if}
      {#if $monitorState !== 'stopped' && !latencyPaused}
        <div class="monitor-stats" title="Web monitor buffer diagnostics — depth is the current worklet ring backlog; target adapts up on underrun and decays back on quiet; catch-ups count how many times we skipped forward to stay in sync with real-time">
          depth {$monitorStats.depthMs.toFixed(0)} ms
          · target {$monitorStats.targetMs.toFixed(0)} ms
          · peak {$monitorStats.peakMs.toFixed(0)} ms
          · catch-ups {$monitorStats.catchupEvents}{#if $monitorStats.catchupEvents > 0} ({$monitorStats.catchupDroppedMs.toFixed(0)} ms dropped){/if}
          · underruns {$monitorStats.underrunBlocks}
        </div>
      {/if}
      <div class="monitor-stats" class:err={$pingBad} title="HTTP round-trip to /healthz — probed globally every {(PING_INTERVAL_MS / 1000).toFixed(0)} s. Web Monitor auto-pauses once the rolling average stays above {PING_DISABLE_MS} ms for {PING_BAD_STREAK} samples in a row (~{(PING_BAD_STREAK * PING_INTERVAL_MS / 1000).toFixed(0)} s), and auto-resumes once it recovers.">
        {#if !$pingStats.sampled}
          ping …
        {:else if $pingStats.err}
          ping · no response
        {:else}
          ping {$pingStats.lastMs.toFixed(0)} ms (avg {$pingStats.avgMs.toFixed(0)} ms)
        {/if}
      </div>

      <label class="sink browser-monitor" title="Add a Sources row for this browser's microphone. Pick a destination from the Sources block above to actually route it into rpg_vox.">
        <input
          type="checkbox"
          checked={$micPref}
          disabled={!browserMicSupported || webMicBusy || $audioAncillary}
          onchange={toggleWebMic}>
        <span>Web microphone input</span>
        <span class="id">
          {#if !browserMicSupported}
            unsupported
          {:else if webMicBusy}
            …
          {:else if $micState.capture === 'active'}
            capturing
          {:else if $micState.presence === 'listening'}
            ready — pick a destination in Sources
          {:else if $micState.presence === 'connecting'}
            connecting…
          {:else if $micPref}
            starting…
          {:else}
            off
          {/if}
        </span>
      </label>
      {#if $micPref && browserMicSupported}
        <label class="sink" title="Choose which of this machine's audio inputs to capture.">
          <span style="flex: 0 0 auto">Input device</span>
          <select value={$micDevice ?? ''} onchange={onDeviceChange}
                  style="flex: 1; margin-left: 8px; min-width: 0;">
            <option value="">Default</option>
            {#each inputDevices as d (d.deviceId)}
              <option value={d.deviceId}>{d.label || `mic ${d.deviceId.slice(0, 8)}…`}</option>
            {/each}
          </select>
        </label>
        <div class="sink" title="How the mic-mute button in the menubar behaves. Toggle mute = sticky (click to flip). Push-to-talk = mic is muted unless you hold the button (or press Space when focused).">
          <span style="flex: 0 0 auto">Mute mode</span>
          <div class="mode-group" role="radiogroup" aria-label="Mute mode" style="margin-left: 8px;">
            <button
              type="button"
              class="mode-btn"
              class:active={$micMode !== 'ptt'}
              role="radio"
              aria-checked={$micMode !== 'ptt'}
              onclick={() => onMuteModeChange('toggle')}
            >Toggle mute</button>
            <button
              type="button"
              class="mode-btn"
              class:active={$micMode === 'ptt'}
              role="radio"
              aria-checked={$micMode === 'ptt'}
              onclick={() => onMuteModeChange('ptt')}
            >Push to talk</button>
          </div>
        </div>
      {/if}
      {#if webMicError}
        <div class="monitor-err">{webMicError}</div>
      {/if}
    </div>
  </section>

  <section class="io-panel">
    <div class="subhead"><span>Server monitor</span>
      <span class="note">piped into a pipewire sink on the server</span>
    </div>
    <div class="sink-list">
      {#if monitorable.length === 0}
        <div class="empty">no pipewire sinks available</div>
      {:else}
        {#each monitorable as s (s.id)}
          <label class="sink">
            <input
              type="radio"
              name="monitor-sink"
              value={s.id}
              checked={$graph?.monitor_sink_id === s.id}
              onchange={() => startMonitor(s.id)}>
            <span>{s.description || s.name}</span>
            <span class="id">#{s.id}</span>
          </label>
        {/each}
      {/if}
    </div>
    <div class="actions">
      {#if $graph?.monitor_sink_id != null}
        <button class="secondary" onclick={stopMonitor}>Stop pipewire monitor</button>
      {/if}
      <button class="secondary" onclick={refreshGraph}>Refresh</button>
    </div>
  </section>

  <section class="io-panel">
    <div class="subhead"><span>Input nodes</span>
      <span class="note">routing is fixed per node — pick one in your app / Helvum</span>
    </div>
    <div class="sink-list">
      <div class="sink">
        <span><code>{$settings?.node_name || 'rpg-vox'}-music</code></span>
        <span class="id">PA → mixed into mic feed</span>
      </div>
      <div class="sink">
        <span><code>{$settings?.node_name || 'rpg-vox'}-vox</code></span>
        <span class="id">processing tap (FX / recording)</span>
      </div>
    </div>
  </section>

  <section class="io-panel">
    <div class="subhead"><span>Who's listening</span></div>
    <div class="listeners">
      {#if !$graph || $graph.listeners.length === 0}
        <div class="empty">nobody</div>
      {:else}
        {#each $graph.listeners as l (l.id)}
          <div class="listener">{l.description || l.name}  ·  #{l.id}</div>
        {/each}
      {/if}
    </div>
  </section>

  <div class="status {status.kind}">{status.text}</div>
  <div class="status {monitorStatus.kind}">{monitorStatus.text}</div>
</div>

<style>
  .mixer { display: flex; flex-direction: column; gap: 12px; padding: 10px; }
  .header { display: flex; justify-content: space-between; align-items: baseline; gap: 12px; }
  h1 { font-size: 20px; margin: 0; }
  h2 { font-size: 14px; margin: 0; font-weight: 600; letter-spacing: 0.02em; }
  .hint { color: var(--muted); font-size: 12px; }

  .sources {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .sources-header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
    flex-wrap: wrap;
  }
  .empty { color: var(--muted); font-size: 12px; padding: 6px 0; }
  .source-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; }
  .source-row {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 12px;
    padding: 8px 10px;
    border-radius: 6px;
    background: rgba(0,0,0,0.15);
    border: 1px solid var(--border);
    transition: border-color 0.15s;
  }
  .source-row.routed { border-color: rgba(122,162,255,0.5); }
  /* Highlight this browser's own web-mic row so it's obvious which
     entry is under this tab's control. Preserved on top of the
     `.routed` styling so the highlight persists when the user has
     picked a destination for their own mic. */
  .source-row.own-web-mic {
    background: rgba(255,213,79,0.12);
    border-color: rgba(255,213,79,0.55);
  }
  .source-row.own-web-mic .source-app { color: rgb(255,213,79); }
  .source-meta { min-width: 0; }
  .source-app { font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.05em; }
  .source-title { font-size: 13px; font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .source-actions { display: flex; flex-shrink: 0; border-radius: 4px; overflow: hidden; flex-wrap: wrap; }
  .route-btn {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 4px 12px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    cursor: pointer;
    min-width: 62px;
    border-radius: 0;
    margin-left: -1px;
  }
  .route-btn:first-child { margin-left: 0; border-top-left-radius: 4px; border-bottom-left-radius: 4px; }
  .route-btn:last-child  { border-top-right-radius: 4px; border-bottom-right-radius: 4px; }
  .route-btn:hover:not(:disabled) { background: rgba(255,255,255,0.05); }
  .route-btn.active {
    background: rgba(122,162,255,0.2);
    border-color: rgba(122,162,255,0.6);
    color: var(--text);
    z-index: 1;
  }
  /* Mute-mode radio pair in the Web Audio section. Reuses the
     `.route-btn` visual style so mode picking feels consistent with
     source routing. */
  .mode-group {
    display: inline-flex;
    border-radius: 4px;
    overflow: hidden;
  }
  .mode-btn {
    background: transparent;
    color: var(--text);
    border: 1px solid var(--border);
    padding: 4px 12px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    cursor: pointer;
    border-radius: 0;
    margin-left: -1px;
  }
  .mode-btn:first-child { margin-left: 0; border-top-left-radius: 4px; border-bottom-left-radius: 4px; }
  .mode-btn:last-child  { border-top-right-radius: 4px; border-bottom-right-radius: 4px; }
  .mode-btn:hover:not(:disabled) { background: rgba(255,255,255,0.05); }
  .mode-btn.active {
    background: rgba(122,162,255,0.2);
    border-color: rgba(122,162,255,0.6);
    color: var(--text);
    z-index: 1;
  }

  .board {
    display: grid;
    grid-auto-flow: column;
    grid-auto-columns: minmax(0, 1fr);
    gap: 8px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 10px;
  }

  .strip {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 12px 6px;
    border-radius: 8px;
    background: rgba(0,0,0,0.15);
    border: 1px solid var(--border);
    transition: opacity 0.15s;
    min-width: 0;
  }
  .strip.master { background: rgba(122,162,255,0.05); border-color: rgba(122,162,255,0.25); }
  .strip.muted { opacity: 0.55; }
  .strip.disabled { opacity: 0.4; }

  .strip-label { text-align: center; width: 100%; }
  .strip-label .name { font-weight: 700; font-size: 14px; }
  .strip-label .sub  { color: var(--muted); font-size: 11px; }
  .name-btn {
    background: transparent;
    border: none;
    color: var(--text);
    font: inherit;
    font-weight: 700;
    font-size: 14px;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name-btn:hover { background: rgba(255,255,255,0.05); }
  .rename-input {
    width: 100%;
    max-width: 110px;
    text-align: center;
    background: rgba(0,0,0,0.3);
    color: var(--text);
    border: 1px solid rgba(122,162,255,0.6);
    border-radius: 4px;
    padding: 2px 6px;
    font: inherit;
    font-weight: 700;
    font-size: 14px;
  }

  .pan-row { display: flex; flex-direction: column; align-items: center; gap: 4px; width: 100%; }
  .pan-row.placeholder { visibility: hidden; height: 48px; }
  .pan-txt { font-size: 11px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .pan { width: 90%; margin: 0; }
  .pan-scale { display: flex; justify-content: space-between; width: 90%; color: var(--muted); font-size: 10px; }

  .fader-row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 8px;
    align-items: stretch;
    height: 200px;
    width: 100%;
    justify-content: center;
  }
  .fader-row.master-fader { grid-template-columns: auto 1fr auto auto; }
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

  .gain-txt { font-variant-numeric: tabular-nums; }
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
  .mute:hover:not(:disabled) { background: rgba(255,255,255,0.05); color: var(--text); }
  .mute.on { background: var(--err); color: #0b0d10; border-color: var(--err); }
  .mute:disabled { opacity: 0.4; cursor: default; }

  .to-mic, .enable-btn {
    padding: 5px 12px;
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-weight: 700;
    font-size: 10px;
    letter-spacing: 0.08em;
    cursor: pointer;
  }
  .to-mic:hover:not(:disabled), .enable-btn:hover { background: rgba(255,255,255,0.05); color: var(--text); }
  .to-mic.on {
    background: rgba(122,162,255,0.2);
    color: var(--text);
    border-color: rgba(122,162,255,0.6);
  }
  .to-mic:disabled { opacity: 0.4; cursor: default; }
  .enable-btn.on {
    background: rgba(46, 204, 74, 0.18);
    color: #b7f0c4;
    border-color: rgba(46, 204, 74, 0.55);
  }

  .io-panel {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .subhead {
    display: flex;
    justify-content: space-between;
    font-weight: 600;
    font-size: 14px;
  }
  .note { color: var(--muted); font-weight: 400; font-size: 12px; }
  .actions { display: flex; gap: 8px; margin-top: 4px; }
  .sink-list, .listeners { display: flex; flex-direction: column; gap: 4px; }
  .sink {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    background: rgba(0,0,0,0.15);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
  }
  .sink input { width: auto; }
  .sink .id { color: var(--muted); margin-left: auto; font-variant-numeric: tabular-nums; }
  .listener {
    padding: 6px 10px;
    background: rgba(0,0,0,0.15);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 13px;
  }
  .monitor-err { color: var(--err); font-size: 12px; padding: 2px 10px; }
  .monitor-stats { color: var(--muted); font-size: 11px; padding: 2px 10px; font-variant-numeric: tabular-nums; }
  .monitor-stats.err { color: var(--err); }
</style>
