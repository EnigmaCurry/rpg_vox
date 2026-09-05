// AudioWorkletProcessor for the browser monitor.
//
// The main thread decodes Opus (via Web Codecs `AudioDecoder`) and posts each
// decoded frame's f32 samples to us via `port`. We buffer them in a small
// ring and drain into the output at the AudioContext's real-time rate.
//
// Underrun → output silence for that block (subscriber falls behind or
// server is between utterances). Overrun → drop the oldest samples so we
// don't accumulate unbounded latency (matches the server-side broadcast
// drops-on-lag policy). Mono in, mirrored to whatever channel count the
// output has (typically stereo).
//
// The ring is sized for ~1 second at 48 kHz — enough to absorb network
// jitter and decoder scheduling wobbles without letting a stalled stream
// pile up minutes of audio.

const RING_FRAMES = 48000;

class MonitorPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.ring = new Float32Array(RING_FRAMES);
    this.read = 0;
    this.write = 0;
    this.filled = 0;
    this.underrunBlocks = 0;

    this.port.onmessage = (ev) => {
      const chunk = ev.data;
      if (!(chunk instanceof Float32Array)) return;
      this.enqueue(chunk);
    };
  }

  enqueue(chunk) {
    const cap = this.ring.length;
    for (let i = 0; i < chunk.length; i++) {
      this.ring[this.write] = chunk[i];
      this.write = (this.write + 1) % cap;
      if (this.filled < cap) {
        this.filled++;
      } else {
        // Overrun: overwrite oldest sample, advance read to match. Better
        // than unbounded latency growth if the stream comes in faster than
        // we can play it out.
        this.read = (this.read + 1) % cap;
      }
    }
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    if (!output || output.length === 0) return true;
    const chan0 = output[0];
    const frames = chan0.length;
    const cap = this.ring.length;

    let hadAudio = false;
    for (let i = 0; i < frames; i++) {
      if (this.filled > 0) {
        chan0[i] = this.ring[this.read];
        this.read = (this.read + 1) % cap;
        this.filled--;
        hadAudio = true;
      } else {
        chan0[i] = 0;
      }
    }
    if (!hadAudio) this.underrunBlocks++;

    // Mirror mono → any additional channels.
    for (let c = 1; c < output.length; c++) {
      output[c].set(chan0);
    }
    return true;
  }
}

registerProcessor('monitor-player', MonitorPlayer);
