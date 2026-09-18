// Cross-tab audio-claim coordination.
//
// Two rpg_vox tabs open in the same browser can't both usefully hold
// the Web audio monitor or Web microphone input at the same time —
// they'd double the bandwidth going to the server, double what the
// user hears through the monitor, and (for the mic) either take turns
// holding the pool slot or fight over it. To avoid that mess, this
// module elects one "primary" tab per browser + origin: whichever tab
// currently has web audio streaming holds the claim, and any tab that
// loads afterward becomes an *ancillary* session that can still
// operate every other control on the page but keeps its Web audio
// prefs off until the primary releases.
//
// Mechanism is a `BroadcastChannel` heartbeat with a deterministic
// tie-break:
//   * On load, a tab sends `hello` and waits ~400 ms for `here`
//     replies. Both messages include the sender's `tabId` and current
//     `hasAudio` bit.
//   * If any peer has `hasAudio: true`, this tab is ancillary.
//   * Else, if any peer has a smaller `tabId`, this tab is ancillary.
//     Purely lexical comparison means both racing tabs agree on the
//     same winner without needing to hear from each other twice.
//   * `beforeunload` posts `bye` so the last tab holding audio
//     doesn't leave stale claims behind.
//
// Coverage note: BroadcastChannel is per-browser-and-origin, so a
// second browser (Chrome next to Firefox) is NOT detected here — it
// gets its own client id and shares the audio path server-side.
// Handling that would require a server-side check; not implemented.

import { writable, get } from 'svelte/store';

const CHANNEL_NAME = 'rpg-vox-audio-claim';
const HELLO_WAIT_MS = 400;

// Per-tab session id. Not persisted — a page reload gets a fresh id,
// which is exactly what we want (a reload IS a new session that should
// re-check whether it can hold audio).
const tabId = (typeof crypto !== 'undefined' && crypto.randomUUID)
  ? crypto.randomUUID()
  : `t-${Math.random().toString(36).slice(2)}-${Date.now()}`;

let channel = null;
let myHasAudio = false;
// Every peer we've heard from → their most recent `hasAudio`. Doesn't
// include our own tabId (we track myHasAudio separately).
const peers = new Map();
let initialCheckResolved = false;

/// Reactive flag: is this tab currently the ancillary one? UI reads
/// this to disable Web audio toggles and hide the mute button.
/// Continues to update live after the initial check — if the primary
/// tab releases, this drops to false and the user can enable audio.
export const audioAncillary = writable(false);

function computeAncillary() {
  // Hold trumps everything. If we're actively holding audio, we're
  // primary regardless of any peer's tabId — the tie-break only
  // decides among tabs still contending, never yanks the role from
  // an established holder. Without this guard, a second tab whose
  // random tabId happened to sort smaller would flip the first tab
  // into ancillary the moment it introduced itself, stealing the
  // audio path from the tab that was already running it.
  if (myHasAudio) {
    audioAncillary.set(false);
    return;
  }
  // Not holding yet — normal election: ancillary if any peer holds
  // audio (they win outright), or if any peer has a smaller tabId
  // (they win the tie-break). Both racing tabs compute the same
  // winner without further coordination.
  let ancillary = false;
  for (const [id, hasAudio] of peers) {
    if (hasAudio) { ancillary = true; break; }
    if (id < tabId) { ancillary = true; break; }
  }
  audioAncillary.set(ancillary);
}

function initChannel() {
  if (channel) return;
  if (typeof BroadcastChannel !== 'function') return;
  channel = new BroadcastChannel(CHANNEL_NAME);
  channel.addEventListener('message', (ev) => {
    const msg = ev.data;
    if (!msg || msg.tabId === tabId) return;
    if (msg.type === 'hello') {
      // Record the new peer at its introduction — even before it
      // reports actual audio state, its tabId contributes to the
      // tie-break so both sides agree on primary/ancillary as soon
      // as they see each other.
      if (!peers.has(msg.tabId)) peers.set(msg.tabId, false);
      safePost({ type: 'here', tabId, hasAudio: myHasAudio });
      computeAncillary();
    } else if (msg.type === 'here' || msg.type === 'state') {
      peers.set(msg.tabId, !!msg.hasAudio);
      computeAncillary();
    } else if (msg.type === 'bye') {
      peers.delete(msg.tabId);
      computeAncillary();
    }
  });
  window.addEventListener('beforeunload', () => {
    safePost({ type: 'bye', tabId });
  });
}

function safePost(msg) {
  if (!channel) return;
  try { channel.postMessage(msg); } catch {}
}

/// One-shot boot check. Sends a `hello` and waits for other tabs to
/// report their state. Resolves with `true` when this tab loses the
/// election (either a peer already holds audio, or a peer has a
/// smaller tabId in the tie-break). After this resolves, the
/// `audioAncillary` store continues to update live.
export async function initialAncillaryCheck() {
  initChannel();
  if (!channel) return false; // no coordination possible; act as primary
  if (initialCheckResolved) {
    computeAncillary();
    return get(audioAncillary);
  }
  safePost({ type: 'hello', tabId });
  await new Promise((r) => setTimeout(r, HELLO_WAIT_MS));
  initialCheckResolved = true;
  computeAncillary();
  return get(audioAncillary);
}

/// Report this tab's current `hasAudio` state to every other tab.
/// Called by browserMonitor when its WS goes to `listening` (and back
/// on stop) and by browserMic when capture goes to `active` (and
/// back), plus by App.svelte at boot to publish intent based on
/// persisted prefs before the WSes actually open. The scalar tracks
/// whichever category flipped — as long as EITHER path is streaming
/// (or intends to), we're a holder.
let monitorClaim = false;
let micClaim = false;
export function setMonitorHasAudio(v) {
  monitorClaim = !!v && !isCurrentlyAncillary();
  publishClaim();
}
export function setMicHasAudio(v) {
  micClaim = !!v && !isCurrentlyAncillary();
  publishClaim();
}
/// True if this tab has already been decided ancillary. Used to
/// refuse `hasAudio: true` broadcasts from ancillary tabs — even if
/// a stray code path attempts capture or opens the monitor WS, its
/// intent must never propagate to peers or it would flip the
/// primary tab into ancillary via the tie-break.
function isCurrentlyAncillary() {
  return get(audioAncillary);
}
function publishClaim() {
  const next = monitorClaim || micClaim;
  if (next === myHasAudio) return;
  myHasAudio = next;
  initChannel(); // idempotent — safe if this is our first publish
  safePost({ type: 'state', tabId, hasAudio: myHasAudio });
  // Our own hold state changed — reevaluate ancillary locally too so
  // the store reflects reality without waiting for a peer message
  // to bounce back.
  computeAncillary();
}
