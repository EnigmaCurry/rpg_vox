// Captures the mic (mono) and posts 20 ms chunks (960 samples at 48 kHz)
// to the main thread, which Opus-encodes them. Adapted from rpg_vox's
// mic-capture worklet. An AudioWorklet rather than the Chrome-only
// MediaStreamTrackProcessor, so Firefox works too.

const FRAME = 960;

class MicCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(FRAME);
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0];
    const ch = input && input[0];
    if (!ch || ch.length === 0) return true;
    let off = 0;
    while (off < ch.length) {
      const take = Math.min(FRAME - this.filled, ch.length - off);
      this.buf.set(ch.subarray(off, off + take), this.filled);
      this.filled += take;
      off += take;
      if (this.filled === FRAME) {
        const out = this.buf;
        this.port.postMessage(out, [out.buffer]);
        this.buf = new Float32Array(FRAME);
        this.filled = 0;
      }
    }
    return true;
  }
}

registerProcessor('mic-capture', MicCapture);
