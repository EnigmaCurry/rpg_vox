import { writable } from 'svelte/store';
import * as api from './api.js';

// localStorage key for the saved pipewire monitor sink. Colocated here (not
// in Settings.svelte) so the boot-time restore and the Settings page share
// exactly one source of truth for the pref.
export const PW_MONITOR_PREF_KEY = 'rpg_vox.monitor_sink_name';

export function getPwMonitorPref() {
  try { return localStorage.getItem(PW_MONITOR_PREF_KEY); } catch { return null; }
}
export function setPwMonitorPref(name) {
  try {
    if (name) localStorage.setItem(PW_MONITOR_PREF_KEY, name);
    else      localStorage.removeItem(PW_MONITOR_PREF_KEY);
  } catch {}
}

// Resident state — survives route changes. Route components pull these and
// call the reload* helpers on mount.

export const health       = writable('checking');   // 'checking' | 'ok' | 'err'
export const settings     = writable(null);
export const workflows    = writable([]);
export const graph        = writable(null);
/// Currently-draining clip on the pipewire mic. Any of the play paths
/// (SpeechInline take, user-memo button, Play All queue) sets this on
/// start and clears it on end. Consumers use it to (a) paint a linear
/// progress overlay on their own DOM element, and (b) scroll themselves
/// into view when they become the active clip. Shape:
///   { widgetId: string, durationMs: number, startedAt: number }
/// or null when nothing is playing.
export const activeClip   = writable(null);
/// Full list of stored agents (name + system prompt). Refreshed on Script
/// mount and after every create/edit so the picker and editor always see
/// the same set.
export const agents       = writable([]);
/// All script summaries [{ id, name, updated_at, ... }]. Populated on Script
/// mount + after every create/rename/delete so the sidebar stays in sync.
export const scripts      = writable([]);
/// Full server-side script: `{ id, turns: [{ id, ord, role, content, blocks: [{ id, ord, text, selected_take, takes: [{id,ord,widget_id,sample_rate,duration_ms}] }] }] }`.
/// Loaded once on Script mount; mutated in place as new turns/takes arrive so
/// route re-mounts don't refetch (turns won't disappear behind the user's back).
export const script       = writable(null);

// --- Health poll (started once from App) ------------------------------------
let healthTimer = null;
export function startHealthPoll() {
  if (healthTimer) return;
  const tick = async () => {
    try {
      const r = await fetch('/healthz');
      health.set(r.ok ? 'ok' : 'err');
    } catch {
      health.set('err');
    }
  };
  tick();
  healthTimer = setInterval(tick, 5000);
}

// --- Settings panel poll (only while /settings is mounted) ------------------
let settingsTimer = null;
export function startSettingsPoll() {
  if (settingsTimer) return;
  const tick = () => {
    reloadGraph().catch(() => {});
    reloadSettings().catch(() => {});
  };
  settingsTimer = setInterval(tick, 3000);
}
export function stopSettingsPoll() {
  if (settingsTimer) { clearInterval(settingsTimer); settingsTimer = null; }
}

// --- Reload helpers ---------------------------------------------------------
export async function reloadSettings()    { settings.set(await api.getSettings()); }
export async function reloadWorkflows()   { workflows.set(await api.listWorkflows()); }
export async function reloadGraph()       { graph.set(await api.getGraph()); }
export async function reloadScript(id)    { script.set(await api.getScript(id)); }
export async function reloadScripts()     { scripts.set(await api.listScripts()); }
export async function reloadAgents(projectId = null) { agents.set(await api.listAgents(projectId)); }

// --- Pipewire monitor auto-restore ------------------------------------------
//
// Called once from App.svelte on mount so the saved monitor sink reconnects
// at boot rather than waiting for the user to navigate to Settings. Match by
// `node.name` because pipewire ids are ephemeral across sessions. The sink
// may not be present in the very first graph snapshot (registry enumeration
// takes a tick), so poll briefly before giving up.
export async function restorePwMonitorFromPref() {
  const wanted = getPwMonitorPref();
  if (!wanted) return;
  // ~3 s total: enough to cover normal startup enumeration without keeping
  // retries alive long enough to fight a user who navigates to Settings and
  // clicks Stop before the sink appears.
  const attempts = 8;
  const delayMs = 400;
  for (let i = 0; i < attempts; i++) {
    let g;
    try { g = await api.getGraph(); } catch { g = null; }
    if (g) {
      graph.set(g);
      // Someone (a previous restore, another client, a leftover session)
      // is already monitoring — no work to do.
      if (g.monitor_sink_id != null) return;
      const match = (g.sinks || []).find((s) => s.name === wanted);
      if (match) {
        try {
          await api.startMonitor(match.id);
          // Refresh so the resident graph store reflects the new
          // monitor_sink_id without waiting for the next Settings poll.
          await reloadGraph();
        } catch (err) {
          console.warn('[pw-monitor] restore failed', err);
        }
        return;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, delayMs));
  }
  // Sink never appeared — that's fine, Settings.svelte's effect will pick it
  // up if the user navigates there and the sink shows up later.
}
