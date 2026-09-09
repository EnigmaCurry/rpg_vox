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

// `configs` is a `[{ speaker?, language?, instruct?, pitchSemitones?,
// timeRatio?, detuneCents?, pan?, gainDb?, delayMs? }, ...]` array — each
// entry is one voice layer, so >1 config fans out into a hive-mind mix.
// An empty array falls back to a single default config on the server.
export const say = (text, configs = []) =>
  jsonPost('/say', { text, configs });

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

// `configs` shape matches `say()` above: array of voice layers, each with
// optional per-config effect fields. The server sends the same camelCase
// field names back for the request payload (see ConfigBody in http.rs).
// Pass `{ signal }` to abort mid-render; the server keeps synthesizing but
// the client stops awaiting so the UI can revert to its pre-render state.
async function widgetRender(method, path, text, configs = [], { signal } = {}) {
  const body = { text, configs };
  const r = await fetch(path, {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    signal,
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

export const createWidget = (text, configs = [], opts = {}) =>
  widgetRender('POST', '/widgets', text, configs, opts);

export const updateWidget = (id, text, configs = [], opts = {}) =>
  widgetRender('PUT', `/widgets/${encodeURIComponent(id)}`, text, configs, opts);

/// Persist a text-only edit to an existing widget row. Used by the "save"
/// action in the SpeakCell edit form so an STT transcript can be corrected
/// without re-synthesizing (and overwriting) a recorded clip.
export async function updateWidgetText(id, text) {
  const r = await fetch(`/widgets/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ text }),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

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

// --- Widget recording ------------------------------------------------------
//
// Two-phase capture from the pipewire `-vox` sink into a widget clip:
//   POST   /widgets/record                 → { session_id }
//   POST   /widgets/record/{sid}/stop      → { id, sample_rate, duration_ms }
//   DELETE /widgets/record/{sid}           → 200 (cancel without persisting)
//
// The stop response mirrors the render endpoints' metadata (widget id +
// sample rate + duration) so SpeakCell can land the recording in the same
// `rendered` state a synthesis would produce, sharing all playback code.

export async function startRecording({ widgetId = null, text = '' } = {}) {
  const body = {};
  if (widgetId) body.widget_id = widgetId;
  if (text) body.text = text;
  const r = await fetch('/widgets/record', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  const parsed = await r.json();
  return { sessionId: parsed.session_id };
}

export async function stopRecording(sessionId) {
  const r = await fetch(`/widgets/record/${encodeURIComponent(sessionId)}/stop`, {
    method: 'POST',
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  const parsed = await r.json();
  return {
    id: parsed.id,
    sampleRate: parsed.sample_rate,
    durationMs: parsed.duration_ms,
    // Present when the server has an STT recognizer loaded; absent
    // (undefined) when STT is disabled. Caller decides whether to
    // overwrite the caption textarea.
    transcript: parsed.transcript,
  };
}

export async function cancelRecording(sessionId) {
  const r = await fetch(`/widgets/record/${encodeURIComponent(sessionId)}`, {
    method: 'DELETE',
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

/// Cancel any TTS clip currently draining through the pipewire mic.
/// Aborting the /widgets/:id/say fetch on its own leaves queued audio
/// draining out for up to ringbuf_seconds — this bumps the server-side
/// stop generation so the pipewire callback empties the ring immediately.
export async function stopPlayback() {
  const r = await fetch('/playback/stop', { method: 'POST' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
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
// --- Script (chat with inline <speak> blocks) -----------------------------
//
// GET    /script                            → full script (turns + blocks + takes)
// POST   /script                            → { text } — append user turn, LLM,
//                                             parse <speak> blocks, return both
//                                             turns.
// DELETE /script                            → clear all turns + take widgets.
// POST   /script/blocks/:id/takes           → { configs? } — synth a new take
//                                             (widget) and auto-select it.
// PATCH  /script/blocks/:id                 → { selectedTake } — pick a take.
// DELETE /script/takes/:id                  → drop a take + its widget.

export const getScript      = ()              => jsonGet('/script');
export const resetScript    = async ()        => {
  const r = await fetch('/script', { method: 'DELETE' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
};

/// Send the user prompt to the LLM. Server persists both turns and parses
/// `<speak>` blocks server-side; response is `{ user_turn, assistant_turn }`
/// with blocks pre-populated (empty takes arrays — client kicks a per-block
/// /takes render to auto-generate the first take).
export async function sendToScript(text) {
  const r = await fetch('/script', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ text }),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Synthesize a fresh take for a speech block. Server reads the block's
/// text from its own row so the client only needs the block id + any voice
/// config layers. Returns the new take (with widget id + duration metadata).
export async function createTake(blockId, configs = []) {
  const r = await fetch(`/script/blocks/${encodeURIComponent(blockId)}/takes`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ configs }),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Pin the block's chosen take by ordinal — mirrors what "audition & keep"
/// means at the UI. The ord is the take row's `ord`, not its index in the
/// takes array (they usually match but a delete can leave gaps).
export async function selectTake(blockId, ord) {
  const r = await fetch(`/script/blocks/${encodeURIComponent(blockId)}`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ selectedTake: ord }),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

/// Drop a take (and its widget + WAV). The server shifts the block's
/// selected_take to the highest remaining ord if the deleted take was the
/// selected one.
export async function deleteTake(takeId) {
  const r = await fetch(`/script/takes/${encodeURIComponent(takeId)}`, {
    method: 'DELETE',
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}
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

// Route an external Stream/Output/Audio node (e.g. a Firefox tab) into
// our music or vox companion sink. `target` is "music" or "vox".
// Server drops any prior link set for the same source before making the
// new one, so calling `linkSource(id, "vox")` after `linkSource(id, "music")`
// safely retargets rather than double-feeding.
export async function linkSource(id, target) {
  const r = await fetch(`/pw/sources/${encodeURIComponent(id)}/link`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ target }),
  });
  const parsed = await r.json().catch(() => null);
  if (!r.ok || parsed?.ok === false) {
    throw new Error(parsed?.error || `HTTP ${r.status}`);
  }
}

// Drop the link set (if any) we previously created for this source.
// Idempotent: unrouted sources succeed silently.
export async function unlinkSource(id) {
  const r = await fetch(`/pw/sources/${encodeURIComponent(id)}/unlink`, {
    method: 'POST',
  });
  const parsed = await r.json().catch(() => null);
  if (!r.ok || parsed?.ok === false) {
    throw new Error(parsed?.error || `HTTP ${r.status}`);
  }
}

// --- Mixer ----------------------------------------------------------------
//
// GET /mixer      → full MixerState snapshot ({ tts, music, vox, master }).
// PUT /mixer      → partial patch; server merges + persists + returns the
//                   new snapshot. Only the changed field(s) need to be sent
//                   (e.g. `{ music: { gain: 0.8 } }`).

export const getMixer = () => jsonGet('/mixer');
export const getMixerLevels = () => jsonGet('/mixer/levels');

export async function updateMixer(patch) {
  const r = await fetch('/mixer', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(patch),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}
