// The tab's audio: plays this person's mix from the room (everyone but
// them, plus the AI) and sends their mic. Both directions are 20 ms mono
// Opus frames at 48 kHz over the room's WebSocket, encoded and decoded
// with WebCodecs. Needs Chrome 94+, Firefox 130+ or Safari 16.4+, and a
// secure page (HTTPS or localhost) for the mic.
//
// What is here is everything the audio needs the page's own thread for:
// the devices, the AudioContext and its nodes, the faders. The frames
// themselves are not -- the socket and both codecs are in a worker (see
// link.js and audio.worker.js), and the only thing this does with them
// is introduce its two worklets to it.

import playerUrl from './player.worklet.js?url';
import micUrl from './mic.worklet.js?url';

const RATE = 48000;
/// How long the room's sound takes to go and to come back, in seconds.
/// See `RoomAudio.hushRoom`.
const HUSH_S = 0.015;

export function supported() {
  return typeof window.AudioDecoder === 'function'
    && typeof window.AudioEncoder === 'function'
    && typeof window.AudioContext === 'function'
    && !!navigator.mediaDevices?.getUserMedia;
}

/// Whether this browser can send the room's sound to a chosen speaker.
/// AudioContext.setSinkId is Chrome's; elsewhere the output follows the
/// system default and the room doesn't offer the choice.
export function canChooseSpeaker() {
  return typeof AudioContext !== 'undefined'
    && typeof AudioContext.prototype.setSinkId === 'function';
}

/// Ask for the microphone once, without keeping it: until permission has
/// been given the device list has ids but no names, so there is nothing
/// to choose between. Returns the error if it was refused.
export async function permitMic() {
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    for (const t of stream.getTracks()) t.stop();
    return null;
  } catch (e) {
    return e?.message || String(e);
  }
}

/// The microphones and speakers to choose from, named where the browser
/// will name them.
export async function listDevices() {
  const devices = await navigator.mediaDevices.enumerateDevices().catch(() => []);
  const of = (kind, fallback) =>
    devices
      .filter((d) => d.kind === kind && d.deviceId)
      .map((d, i) => ({ id: d.deviceId, label: d.label || `${fallback} ${i + 1}` }));
  return {
    mics: of('audioinput', 'Microphone'),
    speakers: canChooseSpeaker() ? of('audiooutput', 'Speaker') : [],
  };
}

/// Tell the room when a device is plugged in or pulled out.
export function onDeviceChange(fn) {
  navigator.mediaDevices?.addEventListener?.('devicechange', fn);
  return () => navigator.mediaDevices?.removeEventListener?.('devicechange', fn);
}

export class RoomAudio {
  constructor(link) {
    // The room's socket and codec, in their worker.
    this.link = link;
    this.ctx = null;
    this.player = null;
    this.volume = null;
    // The room's sound, cut while push to talk is held on half duplex.
    // Its own node rather than the volume one: what you set in Settings
    // is yours to come back to, and a hush that moved the slider would
    // lose it.
    this.hush = null;
    // Whether it is cut right now, so a context started while push to
    // talk is held comes up silent rather than opening at full.
    this.silent = false;
    this.stream = null;
    this._muted = false;
    this.onStats = null;
  }

  /// Whether this person's mic is sending silence. Set on the way in and
  /// whenever it is pressed; the worker is what acts on it, since the
  /// frames are its to fill.
  get muted() {
    return this._muted;
  }

  set muted(on) {
    this._muted = on;
    this.link.setMuted(on);
  }

  /// Cut the room's sound dead, or let it back in.
  ///
  /// Held for as long as push to talk is held, by somebody who has
  /// asked for half duplex -- see `setPttHush`. That is the setting for
  /// speakers with a microphone pointed at them: whatever the room is
  /// saying goes into their mic, is transcribed as theirs, and is read
  /// back to the room as something they said. The mic cannot be made
  /// deaf to one sound in the room it is in, so the room stops making
  /// it. On a headset nothing is cut and this is never called.
  ///
  /// Ramped rather than switched. Zero at a stroke is a click, and a
  /// click is a sound the mic hears too; `HUSH_S` is too short to be a
  /// fade and long enough not to be an edge.
  ///
  /// The frames keep arriving and the player keeps playing them, so
  /// what is missed is missed rather than queued -- a room that caught
  /// up afterwards would be a room talking over itself, a second
  /// behind, every time somebody let go of the key.
  hushRoom(on) {
    this.silent = on;
    const gain = this.hush?.gain;
    if (!gain || !this.ctx) return;
    const now = this.ctx.currentTime;
    gain.cancelScheduledValues(now);
    gain.setValueAtTime(gain.value, now);
    gain.linearRampToValueAtTime(on ? 0 : 1, now + HUSH_S);
  }

  /// Start hearing the room. Call from a click: browsers only start audio
  /// after a gesture.
  async start(volume = 1, speakerId = null) {
    this.ctx = new AudioContext({ sampleRate: RATE, latencyHint: 'interactive' });
    await this.setSpeaker(speakerId);
    if (this.ctx.state === 'suspended') await this.ctx.resume().catch(() => {});
    await this.ctx.audioWorklet.addModule(playerUrl);
    await this.ctx.audioWorklet.addModule(micUrl);
    this.player = new AudioWorkletNode(this.ctx, 'room-player', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
    });
    // The stats come up this port; the mix goes down a port of its own,
    // straight from the worker -- see `Link.usePlayer`.
    this.player.port.onmessage = (ev) => this.onStats?.(ev.data);
    this.volume = new GainNode(this.ctx, { gain: volume });
    this.hush = new GainNode(this.ctx, { gain: this.silent ? 0 : 1 });
    this.player.connect(this.hush).connect(this.volume).connect(this.ctx.destination);
    // A context the browser suspends or interrupts (a modal dialog, an
    // audio device change) is started again at once, or on the next click.
    this.ctx.onstatechange = () => {
      if (!this.ctx) return;
      console.info('[room] audio', this.ctx.state);
      if (this.ctx.state !== 'running' && this.ctx.state !== 'closed') this.resume();
    };
    this.link.usePlayer(this.player);
  }

  /// Play the room through one speaker. A device that has gone away
  /// falls back to the system default rather than failing to start.
  async setSpeaker(id) {
    if (!this.ctx || !canChooseSpeaker()) return;
    try {
      await this.ctx.setSinkId(id || '');
    } catch (e) {
      console.warn('[room] speaker', e);
    }
  }

  /// Start the audio again if the browser stopped it.
  resume() {
    if (this.ctx && this.ctx.state !== 'running' && this.ctx.state !== 'closed') {
      this.ctx.resume().catch(() => {});
    }
  }

  setVolume(v) {
    if (this.volume) this.volume.gain.value = v;
  }

  /// Open the mic and start sending it. Echo cancellation and noise
  /// suppression stay off so the voice comes through as it is (on
  /// speakers, push to talk on half duplex keeps the room's sound out
  /// -- see `setPttHush`).
  async startMic({ deviceId } = {}) {
    if (this.stream) return;
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        deviceId: deviceId ? { exact: deviceId } : undefined,
        channelCount: 1,
        sampleRate: RATE,
        echoCancellation: false,
        noiseSuppression: false,
        autoGainControl: false,
      },
    });
    const src = new MediaStreamAudioSourceNode(this.ctx, { mediaStream: this.stream });
    const cap = new AudioWorkletNode(this.ctx, 'mic-capture', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 1,
      channelCountMode: 'explicit',
    });
    // The frames go from this worklet to the worker and no further up:
    // muting them, encoding them and putting them on the socket are all
    // its, and none of it waits on the page.
    this.link.useMic(cap);
    this.link.setMuted(this._muted);
    src.connect(cap);
    this.micNodes = [src, cap];
  }

  /// What the browser calls the mic it actually gave us. Labels are only
  /// filled in once permission has been granted, which it has by the time
  /// the stream exists.
  async micName() {
    const track = this.stream?.getAudioTracks?.()[0];
    if (!track) return null;
    if (track.label) return track.label;
    // Some platforms leave the track's own label blank; the device list
    // still has it, under the id the track settled on.
    const id = track.getSettings?.().deviceId;
    if (!id) return null;
    try {
      const devices = await navigator.mediaDevices.enumerateDevices();
      return devices.find((d) => d.kind === 'audioinput' && d.deviceId === id)?.label || null;
    } catch {
      return null;
    }
  }

  stopMic() {
    for (const n of this.micNodes || []) n.disconnect();
    this.micNodes = null;
    for (const t of this.stream?.getTracks() || []) t.stop();
    this.stream = null;
    this.link.dropMic();
  }

  async stop() {
    this.stopMic();
    this.link.silence();
    await this.ctx?.close().catch(() => {});
    this.ctx = null;
    this.player = null;
  }
}
