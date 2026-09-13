<script>
  import { onMount, onDestroy } from 'svelte';
  import * as api from '../lib/api.js';
  import { settings } from '../lib/stores.js';

  // Mixer state mirror. Populated from GET /mixer on mount; every knob
  // change PUTs a partial patch and adopts the server's echoed snapshot.
  //   { tts, music, vox: [{enabled, to_output, name, channel: {gain, pan, mute}}, ...], master }
  let mixer = $state(null);
  let status = $state({ text: '', kind: '' });
  let saving = $state(0); // in-flight count so the status can show 'saving…'

  // External Stream/Output/Audio producers visible in the pw graph
  // (browser tabs, media players, etc.). Populated from /pw/graph and
  // refreshed on a slow cadence.
  let sources = $state([]);
  let graphTimer = null;
  const GRAPH_POLL_MS = 2000;

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
    refreshSources();
    graphTimer = setInterval(refreshSources, GRAPH_POLL_MS);
  });

  onDestroy(() => {
    stopLevelPoll();
    if (graphTimer) { clearInterval(graphTimer); graphTimer = null; }
  });

  async function refreshSources() {
    try {
      const g = await api.getGraph();
      sources = g.sources || [];
    } catch {}
  }

  async function routeSource(id, target) {
    try {
      await api.linkSource(id, target);
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `route failed: ${e.message}`, kind: 'err' };
    }
    await refreshSources();
  }

  async function unrouteSource(id) {
    try {
      await api.unlinkSource(id);
      status = { text: '', kind: '' };
    } catch (e) {
      status = { text: `unroute failed: ${e.message}`, kind: 'err' };
    }
    await refreshSources();
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
        connection (usually your default output) untouched.
      </div>
    </div>
    {#if sources.length === 0}
      <div class="empty">no external audio producers currently in the graph</div>
    {:else}
      <ul class="source-list">
        {#each sources as s (s.id)}
          <li class="source-row" class:routed={s.routed_to}>
            <div class="source-meta">
              <div class="source-app">{sourceApp(s)}</div>
              <div class="source-title" title={sourceLabel(s)}>{sourceLabel(s)}</div>
            </div>
            <div class="source-actions" role="radiogroup" aria-label="{sourceLabel(s)} routing">
              <button type="button" class="route-btn"
                class:active={s.routed_to === 'music'}
                onclick={() => routeSource(s.id, 'music')}
                role="radio" aria-checked={s.routed_to === 'music'}>Music</button>
              {#each mixer?.vox ?? [] as slot, i (i)}
                {@const target = slotSuffix(i)}
                <button type="button" class="route-btn"
                  class:active={s.routed_to === target}
                  onclick={() => routeSource(s.id, target)}
                  role="radio" aria-checked={s.routed_to === target}>{slot.name}</button>
              {/each}
              <button type="button" class="route-btn"
                class:active={!s.routed_to}
                onclick={() => unrouteSource(s.id)}
                role="radio" aria-checked={!s.routed_to}
                title={s.kind === 'device'
                  ? 'do not feed this input into rpg_vox'
                  : "drop rpg_vox's routing link; app keeps its default connection"}
              >{s.kind === 'device' ? 'Off' : 'Default'}</button>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>

  <div class="status {status.kind}">{status.text}</div>
</div>

<style>
  .mixer { display: flex; flex-direction: column; gap: 16px; padding: 20px; max-width: 1200px; margin: 0 auto; }
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

  .board {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
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
    width: 110px;
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
</style>
