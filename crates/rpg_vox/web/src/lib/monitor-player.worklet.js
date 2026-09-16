// AudioWorkletProcessor for the browser monitor.
//
// The main thread decodes Opus (via Web Codecs `AudioDecoder`) and posts each
// decoded frame's stereo f32 samples to us via `port` as {left, right}
// planar pairs. We buffer them in two rings and drain into the output at
// the AudioContext's real-time rate.
//
// The jitter target is adaptive:
//   * Baseline ~40 ms. Enough for a smooth link, negligible added latency.
//   * On underrun (block of silence output because the ring was empty),
//     grow the target by the observed silence duration. Repeat starvation
//     ratchets the target up toward MAX_TARGET_FRAMES.
//   * On a quiet window (no underruns in the last stats interval), decay
//     the target back down geometrically. Half-life ~3.4 s.
// The catch-up threshold rides ~160 ms above the current target, so the
// worklet only trims when we're clearly backed up — normal jitter that
// stays under target+margin passes through untouched.
//
// Catch-up itself is a hard read-pointer skip forward until only
// currentTarget frames remain. Instantaneous, but produces a short click.
// Handles all backlog sources: page-load autoplay wall, tab-throttling,
// laptop sleep/wake, long network stalls.
//
// The ring is sized for ~1 second per channel at 48 kHz — a hard ceiling
// on how much we could ever accumulate before catch-up trims.

const RING_FRAMES = 48000;

// Adaptive jitter target bounds.
const MIN_TARGET_FRAMES = 1920;    // ~40 ms — floor when the link is clean.
const MAX_TARGET_FRAMES = 9600;    // ~200 ms — cap so browser doesn't lag
                                   // noticeably behind local monitor. Enough
                                   // headroom for typical WiFi jitter; if
                                   // starvation keeps hitting the cap the
                                   // link is a bigger problem than buffering.

// Trigger the catch-up trim this many frames above the current target.
// Small enough to keep the trim reactive, big enough that ordinary jitter
// hovering just above target doesn't cause repeated skips.
const THRESHOLD_MARGIN_FRAMES = 4800; // ~100 ms

// Multiplier applied to observed silence duration when growing the target.
// >1 gives headroom so a single event doesn't leave us exactly at the edge.
const UNDERRUN_GROWTH_FACTOR = 1.5;

// Geometric decay per quiet stats window. 0.95 = 5% shrink per ~250 ms →
// half-life ~3.4 s → target drifts back from MAX to MIN in ~12 s of calm.
const TARGET_DECAY = 0.95;

// process() runs every 128 frames at 48 kHz (~2.67 ms); ~93 blocks ≈ 250 ms.
const STATS_INTERVAL_BLOCKS = 93;
const AUDIO_BLOCK_FRAMES = 128;

class MonitorPlayer extends AudioWorkletProcessor {
  constructor() {
    super();
    this.left = new Float32Array(RING_FRAMES);
    this.right = new Float32Array(RING_FRAMES);
    this.read = 0;
    this.write = 0;
    this.filled = 0;

    // Windowed peak — reset after each stats emission so the UI shows the
    // max depth observed in the most recent interval, not a stuck high-
    // water mark from a one-time backlog.
    this.peakFilled = 0;

    // Adaptive jitter target — grows on underrun, decays on quiet.
    this.currentTargetFrames = MIN_TARGET_FRAMES;

    // Cumulative counters, reset only on 'reset' or reload.
    this.underrunBlocks = 0;
    this.catchupEvents = 0;
    this.catchupDroppedFrames = 0;

    // Snapshot of underrunBlocks at last emission — subtract to get the
    // delta over this stats window, which drives adaptive growth.
    this.lastEmittedUnderrunBlocks = 0;

    this.blocksSinceStats = 0;

    this.port.onmessage = (ev) => {
      const msg = ev.data;
      if (!msg) return;
      if (msg.type === 'reset') {
        this.read = 0;
        this.write = 0;
        this.filled = 0;
        this.peakFilled = 0;
        this.currentTargetFrames = MIN_TARGET_FRAMES;
        this.underrunBlocks = 0;
        this.catchupEvents = 0;
        this.catchupDroppedFrames = 0;
        this.lastEmittedUnderrunBlocks = 0;
        return;
      }
      if (!(msg.left instanceof Float32Array) || !(msg.right instanceof Float32Array)) return;
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
        // Ring at hard cap — overwrite oldest and advance read so the
        // pointers stay coherent until the threshold trim below kicks in.
        this.read = (this.read + 1) % cap;
      }
    }
    if (this.filled > this.peakFilled) this.peakFilled = this.filled;

    const threshold = this.currentTargetFrames + THRESHOLD_MARGIN_FRAMES;
    if (this.filled > threshold) {
      const drop = this.filled - this.currentTargetFrames;
      this.read = (this.read + drop) % cap;
      this.filled = this.currentTargetFrames;
      this.catchupEvents++;
      this.catchupDroppedFrames += drop;
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

    if (++this.blocksSinceStats >= STATS_INTERVAL_BLOCKS) {
      this.blocksSinceStats = 0;

      // Adapt the jitter target from what we observed this window.
      const deltaUnderruns = this.underrunBlocks - this.lastEmittedUnderrunBlocks;
      this.lastEmittedUnderrunBlocks = this.underrunBlocks;
      if (deltaUnderruns > 0) {
        // Underrun size is a direct measurement of "how much cushion we
        // were missing." Grow by that amount + headroom, capped.
        const silenceFrames = deltaUnderruns * AUDIO_BLOCK_FRAMES;
        const grown = this.currentTargetFrames + Math.round(silenceFrames * UNDERRUN_GROWTH_FACTOR);
        this.currentTargetFrames = Math.min(grown, MAX_TARGET_FRAMES);
      } else {
        const decayed = Math.round(this.currentTargetFrames * TARGET_DECAY);
        this.currentTargetFrames = Math.max(decayed, MIN_TARGET_FRAMES);
      }

      this.port.postMessage({
        type: 'stats',
        depthFrames: this.filled,
        peakFrames: this.peakFilled,
        targetFrames: this.currentTargetFrames,
        underrunBlocks: this.underrunBlocks,
        catchupEvents: this.catchupEvents,
        catchupDroppedFrames: this.catchupDroppedFrames,
      });
      this.peakFilled = this.filled;
    }
    return true;
  }
}

registerProcessor('monitor-player', MonitorPlayer);
