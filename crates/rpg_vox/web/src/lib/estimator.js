// Duration estimator for TTS calls.
//
// Gradio's built-in progress bar uses an EMA of past request durations for
// the same endpoint when the server doesn't stream real progress. We do the
// same, keyed by (input character count) so short and long utterances get
// different baselines. Stored in localStorage so it survives reloads.
//
// The model is intentionally trivial: expected_ms = chars * avg_ms_per_char,
// with per_char updated by an EMA of the last N observations. Not accurate,
// but calibrates itself in a session or two and beats a fixed guess.

const KEY = 'rpg_vox.tts_estimator.v1';
const ALPHA = 0.25;              // EMA weight for new samples
const MIN_MS = 800;              // don't estimate faster than this
const DEFAULT_MS_PER_CHAR = 90;  // cold-start guess (~9s per 100 chars)

function load() {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return null;
    const p = JSON.parse(raw);
    if (typeof p?.ms_per_char !== 'number') return null;
    return p;
  } catch { return null; }
}

function save(state) {
  try { localStorage.setItem(KEY, JSON.stringify(state)); } catch {}
}

export function estimateMs(chars) {
  const state = load();
  const per = state?.ms_per_char ?? DEFAULT_MS_PER_CHAR;
  return Math.max(MIN_MS, Math.round(chars * per));
}

export function recordSample(chars, elapsedMs) {
  if (chars <= 0 || elapsedMs <= 0) return;
  const sample = elapsedMs / chars;
  const state = load();
  const prev = state?.ms_per_char ?? DEFAULT_MS_PER_CHAR;
  const next = state ? prev * (1 - ALPHA) + sample * ALPHA : sample;
  save({ ms_per_char: next, samples: (state?.samples ?? 0) + 1 });
}
