// Browser microphone → server upload.
//
// The mirror of `browserMonitor.js`. When the user enables Web microphone
// input in Web Audio, we open `/mic.ws?client=<uuid>` — a "presence" WS —
// with no mic hardware acquired yet. A row for our client shows up in
// Sources (highlighted yellow) with Off selected by default. When the
// user picks a destination (Music / Vox1 / …), the click serves as the
// user gesture that unlocks `getUserMedia`; we spin up capture, encode
// 20 ms Opus stereo frames via Web Codecs `AudioEncoder`, and stream
// them over the same WS. Off releases the mic hardware AND stops the
// encoder but keeps the WS open (so the source row stays visible until
// the user turns Web microphone input off entirely).
//
// Capture path uses an AudioWorklet (not MediaStreamTrackProcessor,
// which is Chrome-only) so this works in Firefox too. The worklet
// posts fixed-size 20 ms chunks on the main thread; we wrap each in
// an AudioData and feed AudioEncoder.

import { writable, derived } from 'svelte/store';
import { getClientId } from './stores.js';
import { setMicHasAudio } from './audioClaim.js';
import micCaptureWorkletUrl from './mic-capture.worklet.js?url';

const OPUS_SAMPLE_RATE = 48000;
const FRAME_SAMPLES_PER_CHANNEL = 960; // 20 ms — matches the worklet
const OPUS_BITRATE = 96_000;
const PREF_ENABLED_KEY = 'rpg_vox.web_mic_enabled';
const PREF_DEVICE_KEY = 'rpg_vox.web_mic_device_id';
const PREF_MODE_KEY = 'rpg_vox.web_mic_mute_mode';       // 'toggle' | 'ptt'
const PREF_TOGGLE_MUTED_KEY = 'rpg_vox.web_mic_muted';   // 'on' / absent

// Module state — must be declared BEFORE the micEffectiveMuted subscriber
// below, since `subscribe()` fires its callback synchronously with the
// initial value and the callback reads `ws`. Any `let` declared after
// that subscribe would be in the TDZ at first-call time.
let ws = null;
let audioCtx = null;
let mediaStream = null;
let sourceNode = null;
let workletNode = null;
let encoder = null;
let nextTsUs = 0;
// Bump on every start/stop so a startCapture that's awaiting
// getUserMedia can notice a concurrent stopCapture and bail without
// leaving orphan hardware behind. See the myGen check inside
// startCapture.
let captureGen = 0;

/// Mute mode. In `toggle` mode the mute button is sticky — click flips
/// `micToggleMuted` and stays there. In `ptt` mode the mic is always
/// muted unless the user is actively holding the button (pointerdown
/// through pointerup).
export const micMode = writable(readModePref());
function readModePref() {
  try { return localStorage.getItem(PREF_MODE_KEY) === 'ptt' ? 'ptt' : 'toggle'; } catch { return 'toggle'; }
}
export function persistMode(mode) {
  try {
    if (mode === 'ptt') localStorage.setItem(PREF_MODE_KEY, 'ptt');
    else localStorage.removeItem(PREF_MODE_KEY);
  } catch {}
  micMode.set(mode === 'ptt' ? 'ptt' : 'toggle');
  // Reset PTT-pressed on any mode change so switching modes never
  // leaves the mic accidentally hot.
  micPttPressed.set(false);
}

/// Sticky mute state used in toggle mode. Persisted so a reload doesn't
/// silently unmute (or mute) an active mic against the user's last
/// setting. Ignored in PTT mode (see `micEffectiveMuted`).
export const micToggleMuted = writable(readToggleMutedPref());
function readToggleMutedPref() {
  try { return localStorage.getItem(PREF_TOGGLE_MUTED_KEY) === 'on'; } catch { return false; }
}
export function persistToggleMuted(muted) {
  try {
    if (muted) localStorage.setItem(PREF_TOGGLE_MUTED_KEY, 'on');
    else localStorage.removeItem(PREF_TOGGLE_MUTED_KEY);
  } catch {}
  micToggleMuted.set(!!muted);
}

/// Transient "user is currently holding the PTT button" flag. Not
/// persisted — it's an ephemeral pointerdown/pointerup pair. Ignored
/// in toggle mode.
export const micPttPressed = writable(false);

/// The single boolean the rest of the system consults: are we sending
/// silence right now? Derived from mode + the appropriate flag so
/// mode switches never require a fan-out of the "am I muted" bit.
export const micEffectiveMuted = derived(
  [micMode, micToggleMuted, micPttPressed],
  ([$mode, $toggle, $ptt]) => ($mode === 'ptt' ? !$ptt : $toggle),
);

// Mirror the derived store into a plain module variable so the RT
// message handler (which fires every 20 ms) reads a scalar rather than
// re-subscribing per frame. Also send a `{"muted": bool}` text message
// to the server on every change so `/pw/graph` can surface muted state
// on the Sources row — the audio is still zeros regardless, this is
// just so the operator can tell "silent because muted" from "silent
// because nobody's talking".
let effectiveMutedNow = false;
let lastSentMuted = null;
micEffectiveMuted.subscribe((v) => {
  effectiveMutedNow = !!v;
  if (ws && ws.readyState === WebSocket.OPEN && effectiveMutedNow !== lastSentMuted) {
    lastSentMuted = effectiveMutedNow;
    try { ws.send(JSON.stringify({ muted: effectiveMutedNow })); } catch {}
  }
});

/// State the UI observes to decide what to render (starting / listening /
/// off / error). Presence and capture are independent — 'presence' can
/// be listening while 'capture' is off (Web Audio checkbox on but Sources
/// set to Off).
export const micState = writable({
  presence: 'stopped',   // 'stopped' | 'connecting' | 'listening' | 'error'
  capture: 'stopped',    // 'stopped' | 'starting' | 'active' | 'error'
  error: '',
});

/// Persisted intent for the Web microphone input checkbox. Restored on
/// app boot; the Sources-row destination pick is stored server-side
/// (keyed by client uuid), so nothing further to remember client-side.
export const micPref = writable(readPref());
function readPref() {
  try { return localStorage.getItem(PREF_ENABLED_KEY) === 'on'; } catch { return false; }
}
/// Public helper mirroring `browserMonitor.isPrefEnabled`. Used by
/// App.svelte to seed an immediate cross-tab audio claim at boot so
/// races between two tabs loading within the ancillary-check window
/// resolve deterministically.
export function isPrefEnabled() { return readPref(); }
export function persistPref(on) {
  try {
    if (on) localStorage.setItem(PREF_ENABLED_KEY, 'on');
    else localStorage.removeItem(PREF_ENABLED_KEY);
  } catch {}
  micPref.set(!!on);
}

/// Preferred input device id. `null` means "let the browser pick the
/// default"; a MediaDeviceInfo.deviceId string pins a specific input.
export const micDevice = writable(readDevicePref());
function readDevicePref() {
  try { return localStorage.getItem(PREF_DEVICE_KEY) || null; } catch { return null; }
}
export function persistDevice(id) {
  try {
    if (id) localStorage.setItem(PREF_DEVICE_KEY, id);
    else localStorage.removeItem(PREF_DEVICE_KEY);
  } catch {}
  micDevice.set(id || null);
}

export function isSupported() {
  return typeof window !== 'undefined'
    && typeof window.AudioEncoder === 'function'
    && typeof window.AudioData === 'function'
    && typeof window.AudioContext === 'function'
    && !!navigator.mediaDevices?.getUserMedia;
}

/// Called once on app load (from App.svelte). If the user had the Web
/// microphone input toggle on last session, open the presence WS now
/// so the source row shows up in Sources and the server has a chance
/// to re-seed the slot's routing atomic from persisted state. Does
/// NOT acquire the mic — that still needs a user gesture. Callers
/// (App.svelte) wire an $effect on top of $graph.web_mic_sources to
/// arm the gesture listener as soon as a persisted routing appears.
export async function restoreFromPref() {
  if (!isSupported() || !readPref()) return;
  try {
    await startPresence();
  } catch (err) {
    console.warn('[mic] restore-from-pref failed', err);
  }
}

/// Open the presence WebSocket and resolve once it's actually OPEN (not
/// merely CONNECTING). Callers that immediately follow with a
/// `reloadGraph` — App.svelte's boot restore path — need the server to
/// have claimed the pool slot by the time the graph fetch fires so the
/// response's `web_mic_sources` array carries our entry; a fast-return
/// while the socket is still handshaking would race the graph fetch
/// past the server's slot claim. No mic hardware is acquired here.
/// Idempotent — a subsequent call while the WS is already OPEN /
/// CONNECTING reuses the in-flight socket.
export async function startPresence() {
  if (ws) {
    if (ws.readyState === WebSocket.OPEN) return;
    if (ws.readyState === WebSocket.CONNECTING) return waitOpen(ws);
  }
  setPresence('connecting');
  const clientId = getClientId();
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  const socket = new WebSocket(`${scheme}://${location.host}/mic.ws?client=${encodeURIComponent(clientId)}`);
  socket.binaryType = 'arraybuffer';
  ws = socket;
  socket.addEventListener('open', () => {
    setPresence('listening');
    // Seed the server with our current mute state so its /pw/graph
    // response is accurate from the first request — the subscriber
    // only fires on *changes* so a fresh WS otherwise wouldn't hear
    // "already muted" until the user toggles.
    try {
      socket.send(JSON.stringify({ muted: effectiveMutedNow }));
      lastSentMuted = effectiveMutedNow;
    } catch {}
  });
  socket.addEventListener('close', () => {
    // Server may have closed us for pool-full / uuid conflict, or the
    // page went offline. Either way, tear capture down too — nothing
    // downstream will consume the encoded frames without an open WS.
    setPresence('stopped');
    stopCapture().catch(() => {});
    ws = null;
  });
  socket.addEventListener('error', () => {
    setPresence('error', 'websocket error');
  });
  return waitOpen(socket);
}

/// Await a WebSocket's `open` event (or fail on `error` / `close`). One
/// shot: listeners are removed after either resolution path so a later
/// close during normal operation doesn't retro-reject this promise.
function waitOpen(socket) {
  if (socket.readyState === WebSocket.OPEN) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const done = (ok) => {
      socket.removeEventListener('open', onOpen);
      socket.removeEventListener('error', onFail);
      socket.removeEventListener('close', onFail);
      if (ok) resolve(); else reject(new Error('web-mic ws failed before open'));
    };
    const onOpen = () => done(true);
    const onFail = () => done(false);
    socket.addEventListener('open', onOpen);
    socket.addEventListener('error', onFail);
    socket.addEventListener('close', onFail);
  });
}

/// Close the presence WebSocket (and force capture off). This is what
/// the Web Audio checkbox → Off runs.
export async function stopPresence() {
  await stopCapture();
  if (ws) {
    try { ws.close(); } catch {}
    ws = null;
  }
  setPresence('stopped');
}

/// Acquire the mic + start the encoder. Called from the Sources-row
/// destination pick (which supplies the user gesture that unlocks
/// getUserMedia). If capture is already running, no-op — the caller's
/// routing PUT is sufficient to redirect the destination server-side.
export async function startCapture() {
  if (audioCtx) return; // already capturing
  if (!ws || ws.readyState !== WebSocket.OPEN) {
    // Presence must be up so the encoded frames have somewhere to go.
    await startPresence();
  }
  if (!isSupported()) {
    setCapture('error', 'Web Codecs AudioEncoder not available');
    throw new Error('unsupported');
  }
  setCapture('starting');
  const myGen = ++captureGen;
  try {
    const deviceId = readDevicePref();
    const constraints = {
      audio: {
        deviceId: deviceId ? { exact: deviceId } : undefined,
        sampleRate: OPUS_SAMPLE_RATE,
        channelCount: 2,
        echoCancellation: false,
        noiseSuppression: false,
        autoGainControl: false,
      },
    };
    const stream = await navigator.mediaDevices.getUserMedia(constraints);
    if (myGen !== captureGen) {
      // A stopCapture (or another startCapture) landed while we were
      // waiting on the browser's mic permission — abandon this run and
      // release the track we just acquired. Prevents a race where a
      // reload-with-persisted-route arms the gesture listener and the
      // user's very next click happens to be Off in Sources.
      for (const t of stream.getTracks()) {
        try { t.stop(); } catch {}
      }
      return;
    }
    mediaStream = stream;

    // AudioContext pinned to the Opus rate so PCM into the worklet is
    // already at 48 kHz — no resampling needed before the encoder.
    audioCtx = new AudioContext({ sampleRate: OPUS_SAMPLE_RATE, latencyHint: 'interactive' });
    if (audioCtx.state === 'suspended') {
      // Some browsers still hand back a suspended context even when
      // called from a gesture (Firefox occasionally does this when
      // the tab hasn't received a click since load). resume() from
      // inside a gesture-triggered call almost always succeeds.
      try { await audioCtx.resume(); } catch {}
    }
    if (audioCtx.sampleRate !== OPUS_SAMPLE_RATE) {
      console.warn(
        `[mic] AudioContext running at ${audioCtx.sampleRate} Hz, not ${OPUS_SAMPLE_RATE};`,
        'server expects 48 kHz Opus. Audio may be pitch-shifted.'
      );
    }

    await audioCtx.audioWorklet.addModule(micCaptureWorkletUrl);
    sourceNode = new MediaStreamAudioSourceNode(audioCtx, { mediaStream });
    workletNode = new AudioWorkletNode(audioCtx, 'mic-capture', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 2,
      channelCountMode: 'explicit',
      channelInterpretation: 'speakers',
    });

    encoder = new AudioEncoder({
      output: (chunk) => {
        if (!ws || ws.readyState !== WebSocket.OPEN) return;
        const buf = new Uint8Array(chunk.byteLength);
        chunk.copyTo(buf);
        try { ws.send(buf); } catch {}
      },
      error: (err) => {
        console.error('[mic] AudioEncoder error', err);
        setCapture('error', err?.message || 'encoder error');
      },
    });
    // Firefox honours `opus.frameDuration` when present; Chrome accepts
    // it too. Falling back to unspecified would let the encoder pick a
    // default frame size (typically 20 ms already) so this is belt-and-
    // suspenders more than strictly required.
    encoder.configure({
      codec: 'opus',
      sampleRate: OPUS_SAMPLE_RATE,
      numberOfChannels: 2,
      bitrate: OPUS_BITRATE,
      opus: { frameDuration: 20000 },
    });

    workletNode.port.onmessage = (ev) => {
      const { left, right } = ev.data || {};
      if (!left || !right || left.length !== FRAME_SAMPLES_PER_CHANNEL) return;
      // Client-side mute: still encode + transmit a frame so the WS
      // stays in a steady 50 Hz cadence and the server never sees a
      // hitch in the audio flow — we just fill with zeros so the
      // decoded PCM contributes nothing to the mix. The parallel
      // {"muted": true} text message (see the micEffectiveMuted
      // subscriber above) lets the server label the source
      // accordingly.
      if (effectiveMutedNow) {
        left.fill(0);
        right.fill(0);
      }
      // Build a planar-stereo AudioData: L samples then R samples,
      // one Float32 backing buffer. AudioData copies internally so
      // reusing the incoming views is fine, but we still need a
      // contiguous plane layout.
      const planar = new Float32Array(FRAME_SAMPLES_PER_CHANNEL * 2);
      planar.set(left, 0);
      planar.set(right, FRAME_SAMPLES_PER_CHANNEL);
      let ad;
      try {
        ad = new AudioData({
          format: 'f32-planar',
          sampleRate: OPUS_SAMPLE_RATE,
          numberOfChannels: 2,
          numberOfFrames: FRAME_SAMPLES_PER_CHANNEL,
          timestamp: nextTsUs,
          data: planar.buffer,
        });
      } catch (err) {
        console.warn('[mic] AudioData ctor failed', err);
        return;
      }
      nextTsUs += 20_000;
      try {
        encoder?.encode(ad);
      } catch (err) {
        console.warn('[mic] encode failed', err);
      } finally {
        try { ad.close(); } catch {}
      }
    };

    // Wire the mic through the worklet. No connect to destination —
    // we only consume samples, we don't want them heard locally.
    sourceNode.connect(workletNode);

    nextTsUs = 0;
    setCapture('active');
  } catch (err) {
    await stopCapture();
    setCapture('error', err?.message || 'capture init failed');
    throw err;
  }
}

/// Release the mic hardware and stop the encoder. Presence WebSocket
/// stays open (the client's slot remains visible in Sources with Off
/// selected). Called both when the user picks Off in Sources and when
/// they toggle Web microphone input off entirely (the latter also runs
/// stopPresence afterwards).
export async function stopCapture() {
  captureGen++;
  if (encoder) {
    try { await encoder.flush(); } catch {}
    try { encoder.close(); } catch {}
    encoder = null;
  }
  if (workletNode) {
    try { workletNode.port.onmessage = null; } catch {}
    try { workletNode.disconnect(); } catch {}
    workletNode = null;
  }
  if (sourceNode) {
    try { sourceNode.disconnect(); } catch {}
    sourceNode = null;
  }
  if (mediaStream) {
    for (const t of mediaStream.getTracks()) {
      try { t.stop(); } catch {}
    }
    mediaStream = null;
  }
  if (audioCtx) {
    try { await audioCtx.close(); } catch {}
    audioCtx = null;
  }
  nextTsUs = 0;
  setCapture('stopped');
}

function setPresence(next, error = '') {
  micState.update((s) => ({ ...s, presence: next, error: error || '' }));
}
function setCapture(next, error = '') {
  micState.update((s) => ({ ...s, capture: next, error: error || s.error }));
  // Announce claim state across tabs — this browser's mic is a "held"
  // audio path only when capture is actually running.
  setMicHasAudio(next === 'active');
}

// Document-level one-shot gesture listener. Installed by
// `armGestureCapture` when we know the server has a persisted routing
// for this client but capture isn't running yet (typical after a page
// reload: the routing pref restores server-side, the Sources row
// paints the destination as selected, but `getUserMedia` still needs a
// user gesture). First pointerdown/keydown after arming fires
// startCapture. Uses `capture: true` so it runs before app-level
// handlers — the user's click still reaches its intended target (a
// Sources button, a fader, anywhere), we just also awaken the mic.
let armedListener = null;

/// Install the gesture-triggered auto-start. Idempotent — a second
/// call while a listener is already armed is a no-op. Safe to call
/// once capture is already active (still no-op).
export function armGestureCapture() {
  if (armedListener) return;
  const handler = () => {
    document.removeEventListener('pointerdown', handler, true);
    document.removeEventListener('keydown', handler, true);
    armedListener = null;
    startCapture().catch((err) => {
      console.warn('[mic] gesture-triggered capture failed', err);
    });
  };
  armedListener = handler;
  document.addEventListener('pointerdown', handler, true);
  document.addEventListener('keydown', handler, true);
}

/// Remove the gesture listener without firing capture. Useful when the
/// caller notices the routing was cleared (Off in Sources, Web audio
/// checkbox off) before the user got around to a first click.
export function disarmGestureCapture() {
  if (!armedListener) return;
  document.removeEventListener('pointerdown', armedListener, true);
  document.removeEventListener('keydown', armedListener, true);
  armedListener = null;
}

/// Enumerate audio input devices via navigator.mediaDevices. Some browsers
/// only expose full labels after the user has granted a mic permission at
/// least once — devices are still enumerable before that (with empty
/// labels), so we can still populate the dropdown; enabling the mic once
/// re-fetches with labels filled in.
export async function listInputDevices() {
  if (!navigator.mediaDevices?.enumerateDevices) return [];
  try {
    const all = await navigator.mediaDevices.enumerateDevices();
    return all.filter((d) => d.kind === 'audioinput');
  } catch {
    return [];
  }
}
