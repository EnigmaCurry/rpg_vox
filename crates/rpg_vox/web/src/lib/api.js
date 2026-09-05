async function jsonPost(path, body) {
  const r = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const parsed = await r.json().catch(() => null);
  return { ok: r.ok && parsed?.ok !== false, status: r.status, body: parsed };
}

async function jsonGet(path) {
  const r = await fetch(path);
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return await r.json();
}

export const say            = (text, instruct = '') =>
  jsonPost('/say', instruct ? { text, instruct } : { text });

// --- Speak widget CRUD ------------------------------------------------------
//
// Each SpeakCell is backed by a persistent row (see server-side store.rs):
//   POST   /widgets          → creates + synthesizes  → returns id + WAV
//   PUT    /widgets/{id}     → re-synthesizes + saves → returns WAV
//   DELETE /widgets/{id}     → drops row + WAV file
//
// Render endpoints stream audio/wav in the response body. Metadata comes
// back in headers: x-widget-id, x-sample-rate, x-duration-ms.

/// Load an existing widget's cached clip without re-synthesizing. Used by
/// SpeakCell on mount to rehydrate audio when a scene's clip was rendered
/// in an earlier session (widgetId came from the /state blob). 404 means
/// the server-side row or WAV was lost; callers fall back to a fresh render.
export async function fetchWidget(id) {
  const r = await fetch(`/widgets/${encodeURIComponent(id)}`);
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    const err = new Error(detail || `HTTP ${r.status}`);
    err.status = r.status;
    throw err;
  }
  const blob = await r.blob();
  return {
    id: r.headers.get('x-widget-id'),
    blob,
    sampleRate: Number(r.headers.get('x-sample-rate')) || null,
    durationMs: Number(r.headers.get('x-duration-ms')) || null,
  };
}

// Voice params: `voice = { speaker, language, instruct, pitchSemitones,
// timeRatio }` where any subset may be omitted/empty. `instruct` here is the
// per-clip override; if empty, the server falls back to the character's
// default (or the process default). Effect fields are only sent when
// non-identity so the server can short-circuit no-effect renders.
async function widgetRender(method, path, text, instruct, voice = {}) {
  const body = { text };
  const effectiveInstruct = instruct || voice.instruct || '';
  if (voice.speaker) body.speaker = voice.speaker;
  if (voice.language) body.language = voice.language;
  if (effectiveInstruct) body.instruct = effectiveInstruct;
  if (Number.isFinite(voice.pitchSemitones) && voice.pitchSemitones !== 0) {
    body.pitch_semitones = voice.pitchSemitones;
  }
  if (Number.isFinite(voice.timeRatio) && voice.timeRatio !== 1) {
    body.time_ratio = voice.timeRatio;
  }
  const r = await fetch(path, {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  const blob = await r.blob();
  return {
    id: r.headers.get('x-widget-id'),
    blob,
    sampleRate: Number(r.headers.get('x-sample-rate')) || null,
    durationMs: Number(r.headers.get('x-duration-ms')) || null,
  };
}

export const createWidget = (text, instruct = '', voice = {}) =>
  widgetRender('POST', '/widgets', text, instruct, voice);

export const updateWidget = (id, text, instruct = '', voice = {}) =>
  widgetRender('PUT', `/widgets/${encodeURIComponent(id)}`, text, instruct, voice);

/// Ask the server to concatenate a scene's rendered clips into a single FLAC.
/// Returns `{ blob, filename }` — the filename comes from the server's
/// Content-Disposition (scene name + timestamp) so a browser download uses
/// the same name the server logged.
export async function mixScene(sceneName, pauseMs, clipIds) {
  const r = await fetch('/scenes/mix', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      scene_name: sceneName,
      pause_ms: pauseMs | 0,
      clip_ids: clipIds,
    }),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  const blob = await r.blob();
  const disposition = r.headers.get('content-disposition') || '';
  const match = /filename="([^"]+)"/.exec(disposition);
  return { blob, filename: match ? match[1] : `${sceneName || 'scene'}.flac` };
}

/// Play a previously-rendered widget through the pipewire virtual mic. The
/// server blocks until the ring buffer has drained, so this promise resolves
/// when audio has actually finished playing — callers can sequence per-clip
/// calls without extra timing. Pass `{ signal }` to abort mid-playback; note
/// the server keeps pushing whatever's already been queued (the ring may
/// still be draining for a moment after the fetch aborts).
export async function playWidget(id, { signal } = {}) {
  const r = await fetch(`/widgets/${encodeURIComponent(id)}/say`, {
    method: 'POST',
    signal,
  });
  const parsed = await r.json().catch(() => null);
  if (!r.ok || parsed?.ok === false) {
    const err = new Error(parsed?.error || `HTTP ${r.status}`);
    err.status = r.status;
    throw err;
  }
  return parsed || {};
}

export async function deleteWidget(id) {
  const r = await fetch(`/widgets/${encodeURIComponent(id)}`, { method: 'DELETE' });
  const body = await r.json().catch(() => null);
  if (!r.ok || (body && body.ok === false)) {
    throw new Error(body?.error || `HTTP ${r.status}`);
  }
}

// --- Images ---------------------------------------------------------------
//
// POST /images     → { id, src }  (src = `/images/{id}` for use as an <img> src)
// DELETE /images/id → 200; idempotent
//
// Character avatars + reference pictures use these endpoints instead of
// embedding base64 dataURLs in the app state. Old dataURLs from before this
// change continue to work as-is (both are just valid <img> src strings).

export async function uploadImage(file) {
  const r = await fetch('/images', {
    method: 'POST',
    headers: { 'content-type': file.type || 'application/octet-stream' },
    body: file,
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

export async function deleteImage(id) {
  const r = await fetch(`/images/${encodeURIComponent(id)}`, { method: 'DELETE' });
  const body = await r.json().catch(() => null);
  if (!r.ok || (body && body.ok === false)) {
    throw new Error(body?.error || `HTTP ${r.status}`);
  }
}

// --- App state ------------------------------------------------------------
//
// The whole projects/characters/scenes blob lives server-side under a single
// JSON row. UI selection (currently-loaded project/scene) stays in
// localStorage since it's per-browser rather than shared state.

export async function getAppState() {
  const r = await fetch('/state');
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  // Server returns literal `null` when unset; both branches parse fine.
  return await r.json();
}

export async function putAppState(state) {
  const r = await fetch('/state', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(state),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}
export const getChatHistory = ()              => jsonGet('/chat');
export const sendChat       = (text, speak)   => jsonPost('/chat', { text, speak });
export const resetChat      = async ()        => { await fetch('/chat', { method: 'DELETE' }); };
export const getSettings    = ()              => jsonGet('/settings');
export const updateSettings = (patch)         => jsonPost('/settings', patch);
export const listWorkflows  = ()              => jsonGet('/workflows');
export const verifyWorkflow = async ()        => (await fetch('/workflow/verify', { method: 'POST' })).json();
export const warmupWorkflow = ()              => jsonPost('/workflow/warmup', {});
export const getGraph       = ()              => jsonGet('/pw/graph');
export const startMonitor   = (sinkId)        => jsonPost('/pw/monitor', { sink_id: sinkId });
export const stopMonitor    = async ()        => {
  const r = await fetch('/pw/monitor', { method: 'DELETE' });
  const parsed = await r.json().catch(() => null);
  return { ok: r.ok && parsed?.ok !== false, body: parsed };
};
