/// The sound push to talk makes.
///
/// Two beeps: rising while the microphone is opening, falling once it
/// has closed. The rising pair is the point of the thing -- holding the
/// key arms for a quarter of a second before the mic goes live, and a
/// quarter second of silence is a quarter second you talk over. Filled
/// with a sound that ends exactly as the mic opens, it stops being a
/// delay and becomes a cue: speak when it stops.
///
/// It plays *before* the mic opens, never during, which is the whole
/// reason the arming delay exists at all. On a speakerphone a chirp
/// played once you are live goes out to the room with the first word of
/// every sentence.
///
/// On its own `AudioContext`, deliberately, and not through the room's
/// mixer. Half duplex (`setPttHush`) cuts everything the room is
/// playing for as long as you hold -- which is right for the room and
/// exactly wrong for this, since the people who hold the key on
/// speakers are the ones the cue is most for. App volume is the room's
/// too, and a go-signal that can be turned down to nothing is not one.
///
/// The Android app plays its own copy of this, because the volume key
/// it listens for has its own quarter second to fill and the page is
/// not told anything until that is over. The two are kept to the same
/// numbers below so they are the same sound. See ROOM_ANDROID.md.

/// How long the pair lasts, and so how long arming takes.
export const ARM_MS = 250;

const LEAD = 120;   // ms of silence first -- see ROOM_ANDROID.md: on a
                    // phone this is the room a tap gets to be a tap in,
                    // and here it only keeps the two sounds identical
const BEEP = 55;    // ms of each
const GAP = 20;     // ms of silence between
const LOW = 700;
const HIGH = 1050;  // a fifth above: a pleasant pair, and high enough
                    // for a phone to actually produce
const PEAK = 0.35;
const FADE = 0.006; // 6 ms on and off each beep. A sine that starts at
                    // full height is a click, and a click in a headset
                    // is worse than no chirp at all.

let ctx = null;
let playing = [];

/// Made on the first chirp and not before. An `AudioContext` wants a
/// gesture to start, and the first chirp is always one: a key going
/// down or a button being pressed.
function context() {
  if (ctx) return ctx;
  const Ctx = window.AudioContext || window.webkitAudioContext;
  if (!Ctx) return null;
  try {
    ctx = new Ctx();
  } catch {
    return null;
  }
  return ctx;
}

/// Rising to say the microphone is opening, falling to say it closed.
export function chirp(rising) {
  const c = context();
  if (!c) return;
  if (c.state === 'suspended') c.resume().catch(() => {});
  hush();
  const at = c.currentTime + LEAD / 1000;
  beep(c, at, rising ? LOW : HIGH);
  beep(c, at + (BEEP + GAP) / 1000, rising ? HIGH : LOW);
}

function beep(c, at, hz) {
  const osc = c.createOscillator();
  const gain = c.createGain();
  osc.frequency.value = hz;
  const dur = BEEP / 1000;
  const end = at + dur;
  // One curve rather than a handful of ramps. It is the right API for
  // a shape known in advance -- the same shape the app builds by hand
  // into its buffer -- and it keeps this off `linearRampToValueAtTime`,
  // which is the room's own way of moving a gain and is watched as such
  // from outside (see `e2e/ptt.js`). A chirp is not the room's sound
  // and must not be mistaken for it.
  const steps = 32;
  const curve = new Float32Array(steps);
  for (let i = 0; i < steps; i++) {
    const t = (i / (steps - 1)) * dur;
    curve[i] = PEAK * Math.min(1, Math.min(t, dur - t) / FADE);
  }
  gain.gain.setValueCurveAtTime(curve, at, dur);
  osc.connect(gain).connect(c.destination);
  osc.start(at);
  osc.stop(end);
  playing.push(osc);
  osc.onended = () => {
    playing = playing.filter((o) => o !== osc);
  };
}

/// Cut a chirp short. The hold it was about is over before it opened
/// anything, and the rest of a sound meaning `you may speak` is a lie.
export function hush() {
  for (const osc of playing) {
    try { osc.stop(); } catch {}
  }
  playing = [];
}
