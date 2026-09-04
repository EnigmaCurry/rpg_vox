import { writable } from 'svelte/store';
import * as api from './api.js';

// Resident state — survives route changes. Route components pull these and
// call the reload* helpers on mount.

export const health       = writable('checking');   // 'checking' | 'ok' | 'err'
export const settings     = writable(null);
export const workflows    = writable([]);
export const graph        = writable(null);
export const chatHistory  = writable([]);

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
export async function reloadChatHistory() { chatHistory.set(await api.getChatHistory()); }
