<script>
  import { onMount, onDestroy } from 'svelte';
  import {
    settings, workflows, graph,
    reloadSettings, reloadWorkflows, reloadGraph,
    startSettingsPoll, stopSettingsPoll,
  } from '../lib/stores.js';
  import * as api from '../lib/api.js';
  import * as browserMonitor from '../lib/browserMonitor.js';
  import { monitorState } from '../lib/browserMonitor.js';

  const MONITOR_PREF_KEY = 'rpg_vox.monitor_sink_name';

  let status = $state({ text: '', kind: '' });
  let wfBusy = $state(false);
  let selectedWf = $state('');
  // Guard so the saved-monitor restore only fires once per mount. Any explicit
  // click flips this true so the effect doesn't fight the user.
  let monitorRestored = false;

  // Browser monitor is an additive toggle alongside the pipewire sink radio
  // group — selecting it says "stream the tap into THIS tab," which has no
  // bearing on whether other clients also listen or whether a pipewire sink
  // is receiving the same tap. Persisted per-tab in localStorage; auto-
  // restore happens from App.svelte on mount.
  let browserMonitorBusy = $state(false);
  let browserMonitorError = $state('');
  const browserMonitorSupported = browserMonitor.isSupported();
  // Reflect the actual live state so navigating back to Settings shows the
  // checkbox as checked when the monitor is already running. 'awaiting-
  // gesture' also counts as on — the user's intent is fulfilled once
  // they click anywhere.
  const browserMonitorOn = $derived(
    $monitorState === 'listening'
      || $monitorState === 'connecting'
      || $monitorState === 'awaiting-gesture',
  );

  onMount(async () => {
    try {
      await reloadWorkflows();
      await reloadSettings();
      await reloadGraph();
    } catch (e) {
      status = { text: `load error: ${e.message}`, kind: 'err' };
    }
    startSettingsPoll();
  });
  // Deliberately do NOT stop the browser monitor when navigating away — it
  // outlives the Settings component so the user can leave this tab open and
  // still hear the stream. Persistence (Settings toggle → localStorage) +
  // App.svelte's restore-on-mount keep it running across reloads too.
  onDestroy(() => stopSettingsPoll());

  // Keep the select in sync with server settings (initial load + external updates).
  $effect(() => {
    if ($settings) selectedWf = $settings.workflow_name || '';
  });

  // First graph after mount: restore the saved monitor sink if nothing's
  // currently monitored. Match by name (id is ephemeral across sessions).
  $effect(() => {
    const g = $graph;
    if (!g || monitorRestored || g.monitor_sink_id != null) return;
    const wanted = savedMonitorName();
    if (!wanted) return;
    const match = monitorableFrom(g, $settings?.node_name || '').find((s) => s.name === wanted);
    if (match) {
      monitorRestored = true;
      startMonitor(match.id, { restore: true });
    }
  });

  function savedMonitorName() {
    try { return localStorage.getItem(MONITOR_PREF_KEY); } catch { return null; }
  }
  function setSavedMonitorName(name) {
    try {
      if (name) localStorage.setItem(MONITOR_PREF_KEY, name);
      else      localStorage.removeItem(MONITOR_PREF_KEY);
    } catch {}
  }

  // Sinks the user might reasonably want to monitor rpg_vox with. Excludes:
  //   * the auto-patch target (we're already routing to it)
  //   * our own companion sinks (`{node_name}-music`, `{node_name}-vox`) —
  //     those are inputs INTO rpg_vox, not third-party outputs
  function monitorableFrom(g, ownPrefix) {
    const skip = ownPrefix ? `${ownPrefix}-` : null;
    return g.sinks.filter((s) =>
      s.id !== g.auto_patch_sink_id
      && s.name !== g.auto_patch_target
      && (!skip || !s.name.startsWith(skip))
    );
  }

  const isComfy = $derived($settings?.backend === 'comfyui');
  const ownNodePrefix = $derived($settings?.node_name || '');
  const monitorable = $derived($graph ? monitorableFrom($graph, ownNodePrefix) : []);

  const summaryHead = $derived.by(() => {
    const s = $settings;
    if (!s) return '—';
    return s.backend === 'comfyui'
      ? `workflow: ${s.workflow_name || 'placeholder'}`
      : `backend: ${s.backend}`;
  });

  const summaryTail = $derived.by(() => {
    const g = $graph;
    if (!g) return '';
    const patch = g.auto_patch_sink_id != null ? ` · auto→#${g.auto_patch_sink_id}` : '';
    const mon   = g.monitor_sink_id   != null ? ` · monitor→#${g.monitor_sink_id}` : '';
    return ` · ${g.listeners.length} listener(s)${patch}${mon}`;
  });

  const wfSummaryText = $derived.by(() => {
    const w = $settings?.workflow_summary;
    if (!w) return 'no workflow info yet';
    const classes = w.node_classes?.length ? w.node_classes.join(', ') : 'none';
    const warn = w.has_text_placeholder ? '' : '  ⚠ no {{TEXT}} placeholder';
    return `${w.node_count} nodes · classes: ${classes}${warn}`;
  });

  async function chooseWorkflow() {
    wfBusy = true;
    const { ok, body } = await api.updateSettings({ workflow_name: selectedWf });
    wfBusy = false;
    if (!ok) {
      status = { text: `settings error: ${body?.error || 'unknown'}`, kind: 'err' };
    } else {
      settings.set(body);
      status = {
        text: selectedWf ? `workflow: ${selectedWf}` : 'workflow: placeholder',
        kind: 'ok',
      };
    }
  }

  async function verify() {
    status = { text: 'verifying nodes on ComfyUI…', kind: '' };
    try {
      const body = await api.verifyWorkflow();
      if (body.error)                 status = { text: `verify: ${body.error}`, kind: 'err' };
      else if (body.missing?.length)  status = { text: `missing on server: ${body.missing.join(', ')}`, kind: 'err' };
      else                            status = { text: 'all workflow nodes present', kind: 'ok' };
    } catch (e) {
      status = { text: `verify failed: ${e.message}`, kind: 'err' };
    }
  }

  async function warmup() {
    status = { text: 'warming up model…', kind: '' };
    const { ok, body } = await api.warmupWorkflow();
    status = ok
      ? { text: 'warmup complete', kind: 'ok' }
      : { text: `warmup failed: ${body?.error || 'unknown'}`, kind: 'err' };
  }

  async function startMonitor(sinkId, opts = {}) {
    monitorRestored = true;
    const { ok, body } = await api.startMonitor(sinkId);
    if (!ok) {
      status = { text: `monitor error: ${body?.error || 'unknown'}`, kind: 'err' };
    } else {
      const sink = ($graph?.sinks || []).find((s) => s.id === sinkId);
      if (sink?.name) setSavedMonitorName(sink.name);
      status = {
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
      status = { text: `stop error: ${body?.error || 'unknown'}`, kind: 'err' };
    } else {
      setSavedMonitorName(null);
      status = { text: 'monitor stopped', kind: 'ok' };
    }
    await reloadGraph();
  }

  async function toggleBrowserMonitor(ev) {
    // The checkbox event fires with the desired next state. Persistence
    // records user intent (via `persistPref`) so App.svelte can auto-restore
    // on the next page load — Web Audio autoplay policy is handled there
    // by deferring the retry to the first user gesture.
    const wantOn = ev.currentTarget.checked;
    browserMonitorBusy = true;
    browserMonitorError = '';
    try {
      if (wantOn) {
        browserMonitor.persistPref(true);
        await browserMonitor.start((s) => {
          if (s.state === 'error') {
            browserMonitorError = s.detail || 'monitor error';
          }
        });
        status = { text: 'browser monitor on', kind: 'ok' };
      } else {
        browserMonitor.persistPref(false);
        await browserMonitor.stop();
        status = { text: 'browser monitor off', kind: 'ok' };
      }
    } catch (e) {
      // Failure clears the pref so we don't try to auto-restore into the
      // same broken state on the next load.
      browserMonitor.persistPref(false);
      browserMonitorError = e?.message || 'monitor failed';
      status = { text: `browser monitor: ${browserMonitorError}`, kind: 'err' };
    } finally {
      browserMonitorBusy = false;
    }
  }

  async function refresh() {
    try { await Promise.all([reloadSettings(), reloadGraph()]); }
    catch (e) { status = { text: `refresh error: ${e.message}`, kind: 'err' }; }
  }
</script>

<div class="settings">
  <h1>Settings</h1>
  <div class="sum">{summaryHead}{summaryTail}</div>

  {#if isComfy}
    <section>
      <div class="subhead">
        <span>TTS workflow</span>
        <span class="note">{$settings?.workflow_summary?.source || '—'}</span>
      </div>
      <select bind:value={selectedWf} onchange={chooseWorkflow} disabled={wfBusy}>
        <option value="">
          {$workflows.length ? '— placeholder —' : '— placeholder (no workflows in dir) —'}
        </option>
        {#each $workflows as w (w.name)}
          <option value={w.name}>{w.name}  ({w.summary.node_count} nodes)</option>
        {/each}
      </select>
      <div class="wf-summary">{wfSummaryText}</div>
      <div class="actions">
        <button class="secondary" onclick={verify}>Verify nodes</button>
        <button class="secondary" onclick={warmup}>Warm up</button>
      </div>
    </section>
    <hr>
  {/if}

  <section>
    <div class="subhead"><span>Local monitor</span></div>
    <div class="sink-list">
      <!-- Browser monitor: additive, per-tab. Independent of the pipewire
           radio group below — enabling this only affects THIS tab; it says
           nothing about whether another client also has a monitor open. -->
      <label class="sink browser-monitor" title="Stream the tap into this browser tab (does not affect other clients)">
        <input
          type="checkbox"
          checked={browserMonitorOn}
          disabled={!browserMonitorSupported || browserMonitorBusy}
          onchange={toggleBrowserMonitor}>
        <span>Web browser (this tab)</span>
        <span class="id">
          {#if !browserMonitorSupported}
            unsupported
          {:else if browserMonitorBusy}
            …
          {:else if $monitorState === 'awaiting-gesture'}
            click to start
          {:else if browserMonitorOn}
            listening
          {:else}
            off
          {/if}
        </span>
      </label>
      {#if browserMonitorError}
        <div class="monitor-err">{browserMonitorError}</div>
      {/if}

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
      <button class="secondary" onclick={refresh}>Refresh</button>
    </div>
  </section>

  <hr>

  <section>
    <div class="subhead"><span>Input nodes</span>
      <span class="note">routing is fixed per node — pick one in your app / Helvum</span>
    </div>
    <!-- Two dedicated companion sink nodes with fixed routing. Purely
         informational here: users route external apps into whichever node
         matches their intent. -->
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

  <hr>

  <section>
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
</div>

<style>
  .settings { display: flex; flex-direction: column; gap: 14px; }
  h1 { font-size: 20px; margin: 0; }
  .sum {
    font-size: 13px;
    color: var(--muted);
    padding: 8px 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  section { display: flex; flex-direction: column; gap: 8px; }
  .subhead {
    display: flex;
    justify-content: space-between;
    font-weight: 600;
    font-size: 14px;
  }
  .note { color: var(--muted); font-weight: 400; font-size: 12px; }
  .wf-summary { color: var(--muted); font-size: 13px; }
  .actions { display: flex; gap: 8px; margin-top: 4px; }

  .sink-list, .listeners {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .sink {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
  }
  .sink input { width: auto; }
  .sink .id { color: var(--muted); margin-left: auto; font-variant-numeric: tabular-nums; }

  .listener {
    padding: 6px 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 13px;
  }
  .monitor-err { color: var(--err); font-size: 12px; padding: 2px 10px; }
</style>
