// The tab's audio: plays this person's mix from the room (everyone but
// them, plus the AI) and sends their mic. Both directions are 20 ms mono
// Opus frames at 48 kHz over the room's WebSocket, encoded and decoded
// with WebCodecs. Needs Chrome 94+, Firefox 130+ or Safari 16.4+, and a
// secure page (HTTPS or localhost) for the mic.

import playerUrl from './player.worklet.js?url';
import micUrl from './mic.worklet.js?url';

const RATE = 48000;
const FRAME_US = 20000;

export function supported() {
  return typeof window.AudioDecoder === 'function'
    && typeof window.AudioEncoder === 'function'
    && typeof window.AudioContext === 'function'
    && !!navigator.mediaDevices?.getUserMedia;
}

export class RoomAudio {
  constructor(send) {
    // send(bytes): put one encoded mic frame on the WebSocket.
    this.send = send;
    this.ctx = null;
    this.decoder = null;
    this.player = null;
    this.volume = null;
    this.encoder = null;
    this.stream = null;
    this.muted = false;
    this.inTs = 0;
    this.outTs = 0;
    this.onStats = null;
    // Loudest sample of the mix since the last takeHeard().
    this.heard = 0;
  }

  takeHeard() {
    const h = this.heard;
    this.heard = 0;
    return h;
  }

  /// Start hearing the room. Call from a click: browsers only start audio
  /// after a gesture.
  async start(volume = 1) {
    this.ctx = new AudioContext({ sampleRate: RATE, latencyHint: 'interactive' });
    if (this.ctx.state === 'suspended') await this.ctx.resume().catch(() => {});
    await this.ctx.audioWorklet.addModule(playerUrl);
    await this.ctx.audioWorklet.addModule(micUrl);
    this.player = new AudioWorkletNode(this.ctx, 'room-player', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
    });
    this.player.port.onmessage = (ev) => this.onStats?.(ev.data);
    this.volume = new GainNode(this.ctx, { gain: volume });
    this.player.connect(this.volume).connect(this.ctx.destination);
    this.decoder = new AudioDecoder({
      output: (data) => {
        const pcm = new Float32Array(data.numberOfFrames);
        try {
          data.copyTo(pcm, { planeIndex: 0, format: 'f32-planar' });
        } finally {
          data.close();
        }
        let peak = this.heard;
        for (let i = 0; i < pcm.length; i++) peak = Math.max(peak, Math.abs(pcm[i]));
        this.heard = peak;
        this.player?.port.postMessage(pcm, [pcm.buffer]);
      },
      error: (e) => console.error('[room] decoder', e),
    });
    this.decoder.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1 });
  }

  /// One Opus frame of this tab's mix.
  frame(bytes) {
    if (this.decoder?.state !== 'configured') return;
    try {
      this.decoder.decode(new EncodedAudioChunk({ type: 'key', timestamp: this.inTs, duration: FRAME_US, data: bytes }));
      this.inTs += FRAME_US;
    } catch (e) {
      console.warn('[room] decode', e);
    }
  }

  setVolume(v) {
    if (this.volume) this.volume.gain.value = v;
  }

  /// Open the mic and start sending it. Headset mics: echo cancellation
  /// and noise suppression stay off so the voice comes through as it is.
  async startMic(deviceId) {
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
    this.encoder = new AudioEncoder({
      output: (chunk) => {
        const buf = new Uint8Array(chunk.byteLength);
        chunk.copyTo(buf);
        this.send(buf);
      },
      error: (e) => console.error('[room] encoder', e),
    });
    this.encoder.configure({ codec: 'opus', sampleRate: RATE, numberOfChannels: 1, bitrate: 64000, opus: { frameDuration: 20000 } });
    const src = new MediaStreamAudioSourceNode(this.ctx, { mediaStream: this.stream });
    const cap = new AudioWorkletNode(this.ctx, 'mic-capture', {
      numberOfInputs: 1,
      numberOfOutputs: 0,
      channelCount: 1,
      channelCountMode: 'explicit',
    });
    cap.port.onmessage = (ev) => {
      const pcm = ev.data;
      // Muted still sends silence: the room's clock and transcription
      // keep going, and nobody hears a hitch when you unmute.
      if (this.muted) pcm.fill(0);
      if (this.encoder?.state !== 'configured') return;
      const data = new AudioData({
        format: 'f32-planar',
        sampleRate: RATE,
        numberOfChannels: 1,
        numberOfFrames: pcm.length,
        timestamp: this.outTs,
        data: pcm,
      });
      this.outTs += FRAME_US;
      try {
        this.encoder.encode(data);
      } finally {
        data.close();
      }
    };
    src.connect(cap);
    this.micNodes = [src, cap];
  }

  stopMic() {
    for (const n of this.micNodes || []) n.disconnect();
    this.micNodes = null;
    for (const t of this.stream?.getTracks() || []) t.stop();
    this.stream = null;
    try { this.encoder?.close(); } catch {}
    this.encoder = null;
  }

  async stop() {
    this.stopMic();
    try { this.decoder?.close(); } catch {}
    this.decoder = null;
    await this.ctx?.close().catch(() => {});
    this.ctx = null;
    this.player = null;
  }
}
