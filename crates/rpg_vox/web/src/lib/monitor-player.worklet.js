// AudioWorkletProcessor for the browser monitor.
//
// The main thread decodes Opus (via Web Codecs `AudioDecoder`) and posts each
// decoded frame's stereo f32 samples to us via `port` as {left, right}
// planar pairs. We buffer them in two rings and drain into the output at
// the AudioContext's real-time rate.
//
// Underrun → output silence for that block (subscriber falls behind or
// server is between utterances). Overrun → drop the oldest samples so we
// don't accumulate unbounded latency (matches the server-side broadcast
// drops-on-lag policy).
//
// The rings are sized for ~1 second per channel at 48 kHz — enough to
// absorb network jitter and decoder scheduling wobbles without letting a
// stalled stream pile up minutes of audio.

const RING_FRAMES = 48000;

class MonitorPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.left = new Float32Array(RING_FRAMES);
    this.right = new Float32Array(RING_FRAMES);
    this.read = 0;
    this.write = 0;
    this.filled = 0;
    this.underrunBlocks = 0;

    this.port.onmessage = (ev) => {
      const msg = ev.data;
      if (!msg || !(msg.left instanceof Float32Array) || !(msg.right instanceof Float32Array)) return;
      // Both planes are the same length by construction — decoded from the
      // same AudioData. Ignore mismatched pairs defensively.
      const n = Math.min(msg.left.length, msg.right.length);
      this.enqueue(msg.left, msg.right, n);
    };
  }

  enqueue(l, r, n) {
    const cap = this.left.length;
    for (let i = 0; i < n; i++) {
      this.left[this.write] = l[i];
      this.right[this.write] = r[i];
      this.write = (this.write + 1) % cap;
      if (this.filled < cap) {
        this.filled++;
      } else {
        // Overrun: overwrite oldest frame, advance read to match. Better
        // than unbounded latency growth if the stream comes in faster than
        // we can play it out.
        this.read = (this.read + 1) % cap;
      }
    }
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length === 0) return true;
    const outL = output[0];
    const outR = output[1] ?? output[0];
    const frames = outL.length;
    const cap = this.left.length;

    let hadAudio = false;
    for (let i = 0; i < frames; i++) {
      if (this.filled > 0) {
        outL[i] = this.left[this.read];
        outR[i] = this.right[this.read];
        this.read = (this.read + 1) % cap;
        this.filled--;
        hadAudio = true;
      } else {
        outL[i] = 0;
        outR[i] = 0;
      }
    }
    if (!hadAudio) this.underrunBlocks++;

    // If the sink asked for more than 2 channels, mirror L into everything
    // beyond channel 1 (uncommon; typical destinations are stereo).
    for (let c = 2; c < output.length; c++) {
      output[c].set(outL);
    }
    return true;
  }
}

registerProcessor('monitor-player', MonitorPlayer);
