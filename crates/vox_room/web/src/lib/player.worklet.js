// Plays this tab's mix: decoded mono PCM posted from the main thread,
// through an adaptive jitter buffer. Adapted from rpg_vox's
// monitor-player worklet:
//
//   * The target starts at ~40 ms.
//   * Each underrun (an output block of silence because the ring was
//     empty) grows the target by 1.5x the silence, up to ~200 ms.
//   * Each calm stats window (~250 ms) decays it by 5% back toward 40 ms.
//   * When the ring holds more than target + 100 ms (a throttled tab, a
//     network stall), it skips forward to the target at once.

const RING = 48000;
const MIN_TARGET = 1920;
const MAX_TARGET = 9600;
const MARGIN = 4800;
const GROWTH = 1.5;
const DECAY = 0.95;
const STATS_BLOCKS = 93;
const BLOCK = 128;

class Player extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ring = new Float32Array(RING);
    this.read = 0;
    this.write = 0;
    this.filled = 0;
    this.target = MIN_TARGET;
    this.underruns = 0;
    this.lastUnderruns = 0;
    this.blocks = 0;
    this.port.onmessage = (ev) => {
      if (ev.data instanceof Float32Array) this.enqueue(ev.data);
    };
  }

  enqueue(pcm) {
    for (let i = 0; i < pcm.length; i++) {
      this.ring[this.write] = pcm[i];
      this.write = (this.write + 1) % RING;
      if (this.filled < RING) this.filled++;
      else this.read = (this.read + 1) % RING;
    }
    if (this.filled > this.target + MARGIN) {
      const drop = this.filled - this.target;
      this.read = (this.read + drop) % RING;
      this.filled = this.target;
    }
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    if (!out || out.length === 0) return true;
    const ch = out[0];
    let had = false;
    for (let i = 0; i < ch.length; i++) {
      if (this.filled > 0) {
        ch[i] = this.ring[this.read];
        this.read = (this.read + 1) % RING;
        this.filled--;
        had = true;
      } else {
        ch[i] = 0;
      }
    }
    for (let c = 1; c < out.length; c++) out[c].set(ch);
    if (!had) this.underruns++;
    if (++this.blocks >= STATS_BLOCKS) {
      this.blocks = 0;
      const d = this.underruns - this.lastUnderruns;
      this.lastUnderruns = this.underruns;
      this.target = d > 0
        ? Math.min(this.target + Math.round(d * BLOCK * GROWTH), MAX_TARGET)
        : Math.max(Math.round(this.target * DECAY), MIN_TARGET);
      this.port.postMessage({ bufferMs: (this.filled / sampleRate) * 1000, targetMs: (this.target / sampleRate) * 1000 });
    }
    return true;
  }
}

registerProcessor('room-player', Player);
