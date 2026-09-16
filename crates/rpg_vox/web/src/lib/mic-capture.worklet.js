// AudioWorklet that captures interleaved-planar stereo PCM from a live
// input (a getUserMedia MediaStream fed through a MediaStreamAudioSource-
// Node) and posts fixed-size 20 ms chunks to the main thread. The main
// thread wraps each chunk in an `AudioData` and hands it to
// `AudioEncoder` for Opus encoding.
//
// Using an AudioWorklet (rather than the Chrome-only
// MediaStreamTrackProcessor) keeps this cross-browser: Firefox exposes
// AudioWorklet everywhere but doesn't ship MediaStreamTrackProcessor.
//
// The worklet allocates two fresh Float32Arrays per posted frame so it
// can hand ownership to the main thread via a Transferable — cheaper
// than a copy at 50 Hz posting cadence. Allocation on the audio thread
// is normally frowned on, but 3.8 KiB × 2 × 50/s = ~380 KiB/s is well
// under anything that causes underruns in practice.

const FRAME_SAMPLES = 960; // 20 ms @ 48 kHz per channel

class MicCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.leftBuf = new Float32Array(FRAME_SAMPLES);
    this.rightBuf = new Float32Array(FRAME_SAMPLES);
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0];
    if (!input || input.length === 0) return true;
    const l = input[0];
    if (!l || l.length === 0) return true;
    // Mono → duplicate to R so the encoded stereo frame carries the
    // mic uniformly (matches the browser-monitor's stereo path).
    const r = input.length >= 2 ? input[1] : input[0];

    let srcOff = 0;
    const n = l.length;
    while (srcOff < n) {
      const room = FRAME_SAMPLES - this.filled;
      const take = Math.min(room, n - srcOff);
      this.leftBuf.set(l.subarray(srcOff, srcOff + take), this.filled);
      this.rightBuf.set(r.subarray(srcOff, srcOff + take), this.filled);
      this.filled += take;
      srcOff += take;
      if (this.filled === FRAME_SAMPLES) {
        // Transfer ownership to the main thread; allocate fresh
        // buffers for the next fill window.
        const left = this.leftBuf;
        const right = this.rightBuf;
        this.port.postMessage({ left, right }, [left.buffer, right.buffer]);
        this.leftBuf = new Float32Array(FRAME_SAMPLES);
        this.rightBuf = new Float32Array(FRAME_SAMPLES);
        this.filled = 0;
      }
    }
    return true;
  }
}

registerProcessor('mic-capture', MicCaptureProcessor);
