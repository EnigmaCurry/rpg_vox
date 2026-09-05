// Browser-side monitor for the pipewire mic feed.
//
// Server publishes 20 ms Opus frames on /monitor.ws — one message = one
// frame. Here we:
//   1. Open the WebSocket
//   2. Feed each frame into a Web Codecs `AudioDecoder`
//   3. Post decoded f32 PCM to a small AudioWorklet that plays it out at
//      the AudioContext's real-time rate
//
// The AudioContext is created on start() (needs a user gesture), and torn
// down on stop() so a stale monitor isn't holding on to hardware after the
// user unchecks it. Multiple tabs can each open their own monitor — the
// server broadcasts independently to each subscriber.
//
// Web Codecs AudioDecoder is required (Chrome 94+, Firefox 130+, Safari
// 16.4+). `isSupported()` lets callers gate the UI without racing on start.

import { writable } from 'svelte/store';
import workletUrl from './monitor-player.worklet.js?url';

const OPUS_SAMPLE_RATE = 48000;
const FRAME_DURATION_US = 20_000; // 20 ms per Opus frame the server sends
const PREF_KEY = 'rpg_vox.browser_monitor';

let ctx = null;
let node = null;
let ws = null;
let decoder = null;
// Monotonic per-frame timestamp for EncodedAudioChunk. AudioDecoder needs
// strictly-increasing timestamps to keep frames in order across gaps.
let nextTsUs = 0;
let onStatus = null;

/// Reactive state for the monitor. Written whenever the socket / decoder
/// transitions so shared UI (HealthDot, Settings, Menubar) can observe
/// without wiring up its own status callback. Values:
///   'stopped'          — nothing running
///   'connecting'       — start() in flight (or WS opening)
///   'listening'        — WS open AND AudioContext running
///   'awaiting-gesture' — WS open but AudioContext still suspended;
///                        needs a user click/keydown to resume. UI can
///                        surface a prompt in this state.
///   'error'            — WS or decoder failed
export const monitorState = writable('stopped');

// Session-scoped latch: once the user has performed the gesture that
// unlocked Web Audio, browsers remember that for the rest of the tab's
// life — no future ctx.resume() will ever need another click. So the
// "click anywhere to resume" prompt is a strictly one-shot thing tied to
// the initial page load; after that, silently ignore any attempt to put
// us back into 'awaiting-gesture' so a Settings toggle-off/on cycle
// doesn't re-flash the banner.
let gestureUnlocked = false;

function setState(next) {
  if (next === 'listening') gestureUnlocked = true;
  if (next === 'awaiting-gesture' && gestureUnlocked) return;
  monitorState.set(next);
}

/// AudioContext.statechange handler. Fires every time ctx transitions
/// between 'suspended' / 'running' / 'closed'. We use it as the source of
/// truth for the 'listening' vs 'awaiting-gesture' distinction, since ws-
/// open and resume() completion race each other on any start() from a
/// click handler. 'closed' is intentionally ignored — stop() handles that
/// transition directly (setting state → 'stopped') and nulls out `ctx`
/// afterwards, so the module state is already correct by the time this
/// fires for a shutdown.
function onCtxStateChange() {
  if (!ctx) return;
  const wsOpen = ws?.readyState === WebSocket.OPEN;
  if (!wsOpen) return;
  if (ctx.state === 'running') {
    setState('listening');
  } else if (ctx.state === 'suspended') {
    setState('awaiting-gesture');
  }
}

/// Persist the desired on/off state so the monitor auto-restores on the
/// next page load. Called from the toggle handler in Settings, not from
/// start/stop directly — that way an involuntary stop (server closed the
/// socket, decoder error) doesn't clear the user's intent.
export function persistPref(on) {
  try {
    if (on) localStorage.setItem(PREF_KEY, 'on');
    else localStorage.removeItem(PREF_KEY);
  } catch {}
}

function readPref() {
  try {
    return localStorage.getItem(PREF_KEY) === 'on';
  } catch {
    return false;
  }
}

/// Called once on app load. If the user previously turned the monitor on,
/// start it now. Two flavors of autoplay blocking to handle:
///
///   1. `start()` throws (rare — modern browsers usually allow the graph
///      to be constructed without a gesture). Install a gesture listener
///      that retries the full `start()`.
///   2. `start()` succeeds but the `AudioContext` stays `suspended` because
///      `resume()` was called without a user gesture. Everything is wired
///      up correctly — the worklet just isn't ticking. Install a gesture
///      listener that calls `ctx.resume()` on the first click/keydown.
///
/// Case 2 is the common one on fresh page loads.
export async function restoreFromPref() {
  if (!isSupported() || !readPref()) return;
  try {
    await start();
  } catch (err) {
    installGestureListener('retry');
    if (!(err?.name === 'NotAllowedError' || /gesture|user activation/i.test(err?.message || ''))) {
      console.warn('[monitor] restore failed; will retry on first gesture', err);
    }
    return;
  }
  if (ctx && ctx.state !== 'running') {
    installGestureListener('resume');
  }
}

// Guard so we only ever install one gesture listener at a time even if
// restoreFromPref runs multiple times (HMR, remount, etc.).
let gestureListenerInstalled = false;

/// `kind` is either 'retry' (call start()) or 'resume' (call ctx.resume()).
/// One-shot: fires on the first pointerdown or keydown at the document
/// (capture phase so it wins over app handlers), then removes itself.
function installGestureListener(kind) {
  if (gestureListenerInstalled) return;
  gestureListenerInstalled = true;
  const handler = async () => {
    document.removeEventListener('pointerdown', handler, true);
    document.removeEventListener('keydown', handler, true);
    gestureListenerInstalled = false;
    // Pref could have been cleared between initial attempt and gesture
    // (user opened Settings, unchecked the box). Re-check so we don't
    // force it back on.
    if (!readPref()) return;
    try {
      if (kind === 'resume' && ctx) {
        await ctx.resume();
        // Promote state from 'awaiting-gesture' → 'listening' now that
        // the context is really running. The WS `open` handler may have
        // set 'awaiting-gesture'; do it here so the streaming indicator
        // flips on immediately without waiting for anything else.
        if (ws?.readyState === WebSocket.OPEN && ctx.state === 'running') {
          setState('listening');
        }
      } else {
        await start();
      }
    } catch (e) {
      console.warn('[monitor] gesture handler failed', e);
    }
  };
  document.addEventListener('pointerdown', handler, true);
  document.addEventListener('keydown', handler, true);
}

export function isSupported() {
  return typeof window !== 'undefined'
    && typeof window.AudioDecoder === 'function'
    && typeof window.EncodedAudioChunk === 'function'
    && typeof window.AudioContext === 'function';
}

/// Start monitoring. `statusCb` receives {state, detail?} transitions:
///   'connecting' → 'listening' → 'stopped' / 'error'
export async function start(statusCb) {
  if (ctx) return; // already running; no-op keeps callers simple
  onStatus = statusCb || (() => {});
  setState('connecting');
  onStatus({ state: 'connecting' });

  try {
    if (!isSupported()) {
      throw new Error('Web Codecs AudioDecoder not available in this browser');
    }

    ctx = new AudioContext({ sampleRate: OPUS_SAMPLE_RATE, latencyHint: 'interactive' });
    // Follow ctx state changes so `monitorState` reflects reality even when
    // resume() completes asynchronously *after* the WS 'open' event has
    // already fired. Without this, a start() called from a click can race
    // WS-open vs resume-complete and leave the state stuck at
    // 'awaiting-gesture' even though audio is playing correctly.
    ctx.addEventListener('statechange', onCtxStateChange);
    // Kick a resume() but DO NOT await it. Firefox leaves the promise
    // pending indefinitely when there's no user gesture (Chrome rejects
    // fast), which would deadlock the whole start() flow — restoreFromPref
    // would never get to check ctx.state or install its gesture listener.
    // The Settings toggle path already runs inside a click handler so
    // resume() succeeds there and the statechange listener flips us to
    // 'listening'; the auto-restore path leaves ctx.state === 'suspended'
    // for the caller to notice.
    if (ctx.state === 'suspended') {
      ctx.resume().catch(() => {});
    }

    // Warn (once) if the browser downgraded us off 48 kHz — the server
    // sends 48 kHz PCM through Opus and we don't resample here. Off-rate
    // playback would sound pitched.
    if (ctx.sampleRate !== OPUS_SAMPLE_RATE) {
      console.warn(
        `[monitor] AudioContext running at ${ctx.sampleRate} Hz, not ${OPUS_SAMPLE_RATE};`,
        'audio may be pitch-shifted. This typically happens when the OS audio hardware',
        'is fixed to a non-48kHz rate.',
      );
    }

    await ctx.audioWorklet.addModule(workletUrl);
    node = new AudioWorkletNode(ctx, 'monitor-player', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [1],
    });
    node.connect(ctx.destination);

    nextTsUs = 0;
    decoder = new AudioDecoder({
      output: (audioData) => {
        // Opus frames from the server are mono. Copy the single plane into
        // a fresh Float32Array (transferable) and hand it to the worklet.
        const nFrames = audioData.numberOfFrames;
        const buf = new Float32Array(nFrames);
        try {
          audioData.copyTo(buf, { planeIndex: 0, format: 'f32-planar' });
        } finally {
          audioData.close();
        }
        // Transfer the underlying buffer to the worklet to skip a copy.
        node?.port.postMessage(buf, [buf.buffer]);
      },
      error: (err) => {
        console.error('[monitor] AudioDecoder error', err);
        setState('error');
        onStatus({ state: 'error', detail: err?.message || 'decoder error' });
      },
    });
    decoder.configure({
      codec: 'opus',
      sampleRate: OPUS_SAMPLE_RATE,
      numberOfChannels: 1,
    });

    const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
    ws = new WebSocket(`${scheme}://${location.host}/monitor.ws`);
    ws.binaryType = 'arraybuffer';

    ws.addEventListener('open', () => {
      // If the AudioContext is still suspended (autoplay policy), we're
      // wired up but not actually playing audio — surface that as its
      // own state so the UI can prompt for a gesture instead of falsely
      // claiming we're streaming.
      if (ctx && ctx.state === 'running') {
        setState('listening');
      } else {
        setState('awaiting-gesture');
      }
      onStatus({ state: 'listening' });
    });
    ws.addEventListener('message', (ev) => {
      if (!decoder || decoder.state !== 'configured') return;
      // Opus frames are all independently decodable — 'key' for every
      // chunk keeps the decoder happy across broadcast lag drops.
      const chunk = new EncodedAudioChunk({
        type: 'key',
        timestamp: nextTsUs,
        duration: FRAME_DURATION_US,
        data: ev.data,
      });
      nextTsUs += FRAME_DURATION_US;
      try {
        decoder.decode(chunk);
      } catch (err) {
        console.error('[monitor] decode failed', err);
      }
    });
    ws.addEventListener('close', () => {
      setState('stopped');
      onStatus({ state: 'stopped' });
    });
    ws.addEventListener('error', () => {
      setState('error');
      onStatus({ state: 'error', detail: 'websocket error' });
    });
  } catch (err) {
    // Roll back partial init so retry is clean.
    await stop();
    setState('error');
    onStatus?.({ state: 'error', detail: err?.message || 'monitor init failed' });
    throw err;
  }
}

export async function stop() {
  if (ws) {
    try { ws.close(); } catch {}
    ws = null;
  }
  if (decoder) {
    try { decoder.close(); } catch {}
    decoder = null;
  }
  if (node) {
    try { node.disconnect(); } catch {}
    node = null;
  }
  if (ctx) {
    try { await ctx.close(); } catch {}
    ctx = null;
  }
  nextTsUs = 0;
  setState('stopped');
  onStatus?.({ state: 'stopped' });
  onStatus = null;
}

export function isRunning() {
  return !!ctx;
}
