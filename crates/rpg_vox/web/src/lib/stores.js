import { writable, derived } from 'svelte/store';
import * as api from './api.js';

// localStorage key for the saved pipewire monitor sink. Colocated here (not
// in Settings.svelte) so the boot-time restore and the Settings page share
// exactly one source of truth for the pref.
export const PW_MONITOR_PREF_KEY = 'rpg_vox.monitor_sink_name';

// Stable per-browser client id used by `/mic.ws?client=<uuid>` and the
// web-mic routing endpoints. Persisted in localStorage so a page reload
// keeps the same slot and the server can restore the client's saved
// routing pref. Each browser tab in the same profile shares one id —
// only one mic can be captured per tab anyway, and separate profiles /
// private windows naturally get distinct ids since their localStorage
// is isolated.
export const CLIENT_ID_KEY = 'rpg_vox.client_id';
export function getClientId() {
  try {
    let id = localStorage.getItem(CLIENT_ID_KEY);
    if (id && /^[0-9a-f-]{36}$/i.test(id)) return id;
    id = (crypto.randomUUID && crypto.randomUUID()) || generateFallbackUuid();
    localStorage.setItem(CLIENT_ID_KEY, id);
    return id;
  } catch {
    // localStorage disabled — hand out an ephemeral id so the app still
    // works, at the cost of losing routing pref persistence.
    return generateFallbackUuid();
  }
}
function generateFallbackUuid() {
  // RFC 4122 v4 shape from Math.random — only used when crypto.randomUUID
  // is missing (very old browsers) and localStorage is blocked.
  const hex = () => Math.floor(Math.random() * 16).toString(16);
  const s = Array.from({ length: 32 }, hex).join('');
  return `${s.slice(0,8)}-${s.slice(8,12)}-4${s.slice(13,16)}-${(8 + Math.floor(Math.random()*4)).toString(16)}${s.slice(17,20)}-${s.slice(20,32)}`;
}

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
/// Cross-route recording status. Set by the poll loop below whenever
/// GET /record's `activeRecording` field changes shape. Consumers can
/// subscribe from anywhere (Menubar, Record page, future badges) without
/// each one running its own poll timer. Shape when recording:
///   { active: true, id, name, startedAt, durationMs }
/// Shape when idle:
///   { active: false }
export const recordingStatus = writable({ active: false });
export const settings     = writable(null);
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

// --- Health + ping poll (started once from App) -----------------------------
//
// One /healthz probe drives both the online/offline indicator and the ping
// meter — no need to double the request rate. The ping side feeds the
// browser-monitor auto-pause: if the rolling average RTT stays above
// PING_DISABLE_MS for PING_BAD_STREAK consecutive samples, `pingBad`
// flips true and the Web Monitor stops until the link recovers, without
// touching the user's pref.
export const PING_INTERVAL_MS = 2000;
export const PING_HISTORY = 5;
export const PING_DISABLE_MS = 500;
export const PING_BAD_STREAK = 3;

const pingHistory = [];
let pingBadStreak = 0;

export const pingStats = writable({
  lastMs: 0,
  avgMs: 0,
  sampled: false,
  err: false,
  badStreak: 0,
});

export const pingBad = derived(
  pingStats,
  (s) => s.sampled && s.badStreak >= PING_BAD_STREAK,
);

function recordPingOk(rtt) {
  pingHistory.push(rtt);
  while (pingHistory.length > PING_HISTORY) pingHistory.shift();
  const avg = pingHistory.reduce((a, b) => a + b, 0) / pingHistory.length;
  if (avg > PING_DISABLE_MS) pingBadStreak++;
  else pingBadStreak = 0;
  pingStats.set({
    lastMs: rtt,
    avgMs: avg,
    sampled: true,
    err: false,
    badStreak: pingBadStreak,
  });
}

function recordPingErr() {
  // Total failure is at least as bad as high latency — count it toward the
  // streak so persistent /healthz failures also trip the auto-pause path.
  pingBadStreak++;
  pingStats.update((s) => ({ ...s, sampled: true, err: true, badStreak: pingBadStreak }));
}

let healthTimer = null;
export function startHealthPoll() {
  if (healthTimer) return;
  const tick = async () => {
    const t0 = performance.now();
    try {
      const r = await fetch('/healthz', { cache: 'no-store' });
      const rtt = performance.now() - t0;
      if (r.ok) {
        health.set('ok');
        recordPingOk(rtt);
      } else {
        health.set('err');
        recordPingErr();
      }
    } catch {
      health.set('err');
      recordPingErr();
    }
  };
  tick();
  healthTimer = setInterval(tick, PING_INTERVAL_MS);
}

// --- Recording status poll (started once from App) --------------------------
//
// One second is fast enough for the Menubar's hh:mm timer to feel live and
// for the RECORDING banner to appear promptly after Start/Stop; slower
// than that would let the timer visibly lag the wall clock. The Record
// page still polls faster on its own for the transcript list — this
// timer is only for the cross-route status header.
let recordingTimer = null;
export function startRecordingPoll() {
  if (recordingTimer) return;
  const tick = async () => {
    let state = null;
    try {
      state = await api.getRecordState();
    } catch {
      return; // keep the previous value; network hiccup is transient
    }
    const ar = state?.activeRecording ?? null;
    if (!ar) {
      recordingStatus.set({ active: false });
      return;
    }
    recordingStatus.set({
      active: true,
      id: ar.id,
      name: ar.name,
      startedAt: (ar.created_at ?? ar.createdAt) ?? null,
      durationMs: ar.duration_ms ?? ar.durationMs ?? 0,
    });
  };
  tick();
  recordingTimer = setInterval(tick, 1000);
}

// --- Reload helpers ---------------------------------------------------------
export async function reloadSettings()    { settings.set(await api.getSettings()); }
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
