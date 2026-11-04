// Captures the mic (mono) and posts 20 ms chunks (960 samples at 48 kHz)
// straight to the worker that Opus-encodes them, down a port handed over
// at startup -- the page's own thread is not in the path. Adapted from
// rpg_vox's mic-capture worklet. An AudioWorklet rather than the
// Chrome-only MediaStreamTrackProcessor, so Firefox works too.

const FRAME = 960;

class MicCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(FRAME);
    this.filled = 0;
    // Where the frames go, once there is somewhere. Until the port
    // arrives nobody is encoding anything, so what is heard before then
    // is dropped rather than kept to be sent late.
    this.out = null;
    this.port.onmessage = (ev) => {
      if (ev.data?.pcm instanceof MessagePort) this.out = ev.data.pcm;
    };
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
        this.out?.postMessage(out, [out.buffer]);
        this.buf = new Float32Array(FRAME);
        this.filled = 0;
      }
    }
    return true;
  }
}

registerProcessor('mic-capture', MicCapture);
