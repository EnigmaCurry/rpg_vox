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
// `projectId` (optional) selects which project's TTS dictionary applies
// before synthesis; omit to skip dictionary substitution entirely.
export const say = (text, configs = [], projectId = null) => {
  const body = { text, configs };
  if (projectId) body.projectId = projectId;
  return jsonPost('/say', body);
};

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
// Pass `{ characterId, profileId }` to have the server resolve the voice
// profile server-side (correct clone/design mode dispatch) — the `configs`
// argument is ignored in that case.
async function widgetRender(method, path, text, configs = [], { signal, characterId, profileId, projectId } = {}) {
  const body = { text, configs };
  if (characterId) body.characterId = characterId;
  if (profileId) body.profileId = profileId;
  if (projectId) body.projectId = projectId;
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

/// Toggle the wait-fill "computer is thinking" click bed. Enabled at the
/// start of a Script send so Discord participants get a soft mechanical
/// click cue while the LLM is deliberating, disabled once real assistant
/// speech begins playback. `preset` picks a voicing — currently the
/// server only knows `"vintage"`; unknown ids come back as 400.
/// Fire-and-forget: transport errors are swallowed since a failed cue
/// shouldn't derail the surrounding Send.
export async function startClicks(preset = 'vintage') {
  try {
    await fetch('/clicks/start', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ preset }),
    });
  } catch {}
}
export async function stopClicks() {
  try { await fetch('/clicks/stop', { method: 'POST' }); } catch {}
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

// --- Scripts (multi-conversation management) ------------------------------
//
// GET    /scripts               → [{ id, name, updated_at, ... }]
// POST   /scripts { name? }     → { id, name, ... } — created empty
// GET    /scripts/:id           → full script (turns + blocks + takes)
// PATCH  /scripts/:id { name }  → rename
// DELETE /scripts/:id           → delete (cascades everything)
// DELETE /scripts/:id/turns     → clear turns but keep script
// POST   /scripts/:id/user      → phase 1 of send
// POST   /scripts/:id/reply     → phase 2 of send
// POST   /scripts/:id/title     → LLM-generate a new name from the turns

export const listScripts    = ()                     => jsonGet('/scripts');
export const getScript      = (id)                   => jsonGet(`/scripts/${encodeURIComponent(id)}`);

export async function createScript(name = null) {
  const body = {};
  if (name) body.name = name;
  const r = await fetch('/scripts', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

export async function renameScript(id, name) {
  const r = await fetch(`/scripts/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ name }),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

export async function deleteScript(id) {
  const r = await fetch(`/scripts/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

export async function resetScript(id) {
  const r = await fetch(`/scripts/${encodeURIComponent(id)}/turns`, { method: 'DELETE' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

/// Ask the server to synthesize a short title from the script's existing
/// turns and rename the script to it. Returns { name } with the new name.
/// Best-effort — clients call this fire-and-forget after the first reply
/// lands and refresh their sidebar on the next tick.
export async function generateScriptTitle(id) {
  const r = await fetch(`/scripts/${encodeURIComponent(id)}/title`, {
    method: 'POST',
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Phase 1 of the /script two-step. Persists a user turn (with widget or
/// GM-voice proxy synth) and pushes the text into the LLM history. Returns
/// as soon as the GM synth finishes so the client can begin playing the
/// proxy while phase 2 (the LLM call) runs in parallel.
///
/// `widgetId` (optional) — from a `/widgets/record` stop response; when
/// present the server attaches it instead of synthesizing a proxy.
export async function sendUserTurn(scriptId, text, widgetId = null, agentId = null, projectId = null) {
  const body = { text };
  if (widgetId) body.widgetId = widgetId;
  if (agentId) body.agentId = agentId;
  if (projectId) body.projectId = projectId;
  const r = await fetch(`/scripts/${encodeURIComponent(scriptId)}/user`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Edit a user turn in place. Server-side this truncates the transcript
/// from `turnId` onward (dropping every later turn + its widgets), synths
/// a fresh GM-proxy widget for the new text, and creates a replacement
/// user turn. Returns the new `user_turn` in the same shape as
/// `sendUserTurn`. Caller is expected to fire `sendAssistantReply` after
/// to regenerate the LLM response against the rewound history.
/// Re-synth the GM-proxy widget attached to an existing user turn WITHOUT
/// truncating the transcript. Server reads the turn's stored text and
/// regenerates the widget against the current project dictionary + the
/// agent's User voice slot, deletes the old widget row/WAV, and returns
/// the new widget id (or null when silenced / empty text). Used by the
/// Script page's shift-click bulk re-render so user turns pick up the
/// same voice/dictionary edits as the assistant takes.
export async function resynthUserTurn(scriptId, turnId, agentId = null, projectId = null) {
  const body = {};
  if (agentId) body.agentId = agentId;
  if (projectId) body.projectId = projectId;
  const r = await fetch(
    `/scripts/${encodeURIComponent(scriptId)}/turns/${encodeURIComponent(turnId)}/resynth`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    },
  );
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

export async function editUserTurn(scriptId, turnId, text, agentId = null, projectId = null) {
  const body = { text };
  if (agentId) body.agentId = agentId;
  if (projectId) body.projectId = projectId;
  const r = await fetch(
    `/scripts/${encodeURIComponent(scriptId)}/turns/${encodeURIComponent(turnId)}/edit-user`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    },
  );
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Phase 2 of the /script two-step. Calls the LLM against the history that
/// phase 1 populated, persists the assistant turn (with narrator/character
/// blocks parsed out), returns it.
///
/// `agentId` (optional) — which stored agent's system prompt to use for
/// the LLM call. Missing/unknown ids fall back to the built-in Default.
export async function sendAssistantReply(scriptId, agentId = null) {
  const body = {};
  if (agentId) body.agentId = agentId;
  const r = await fetch(`/scripts/${encodeURIComponent(scriptId)}/reply`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

// --- Agents (named system-prompt profiles for /script) --------------------
//
// GET    /agents?projectId=X → [{ id, name, system_prompt, project_id,
//                                voice_user, voice_narrator, voice_character,
//                                read_only, ... }]
// GET    /agents/:id         → single agent
// POST   /agents             → { name, projectId? } — creates blank agent
// PUT    /agents/:id         → patch (name/systemPrompt/voice*), 403 on Default
// DELETE /agents/:id         → 200 (or 403 on Default)

/// List agents. `projectId` (optional) filters to that project plus the
/// built-in Default (which has no project). Omit to fetch every agent.
export function listAgents(projectId = null) {
  const path = projectId
    ? `/agents?projectId=${encodeURIComponent(projectId)}`
    : '/agents';
  return jsonGet(path);
}

export async function createAgent(name, projectId = null) {
  const body = { name };
  if (projectId) body.projectId = projectId;
  const r = await fetch('/agents', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Patch an agent. `patch` is an object with any subset of:
///   { name, systemPrompt, voiceUser, voiceNarrator, voiceCharacter,
///     interstitial }
/// Voice slots take a character id (string) to set, `null` to clear back
/// to the DSP-preset fallback, or the field is omitted to leave alone.
/// `interstitial` follows the same tri-state: a preset id string turns
/// the wait-fill click bed on with that voicing, `null` disables it,
/// omitting the key leaves the current selection alone.
export async function updateAgent(id, patch) {
  const r = await fetch(`/agents/${encodeURIComponent(id)}`, {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(patch),
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

export async function deleteAgent(id) {
  const r = await fetch(`/agents/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

/// Synthesize a fresh take for a speech block. Server reads the block's
/// text from its own row so the client only needs the block id + any voice
/// config layers. Returns the new take (with widget id + duration metadata).
/// Sentinel value the agent-editor dropdown uses to explicitly silence
/// a role. Mirrored on the server (http.rs::VOICE_SILENCE). Distinct
/// from the empty-string "unset — use DSP fallback" state, so custom
/// agents can opt out of TTS for a role while Default's zero-config
/// fallback stays intact.
export const VOICE_SILENCE = '__silence__';

// --- Voice-prompt files (Qwen3 clone-mode save_prompt roundtrip) ---------
//
// POST /voices  → { id, filename, size, transcript } — server does the
//                 Gradio round-trip and stores the compact voice-prompt
//                 file. `transcript` echoes whatever ref_txt actually fed
//                 save_prompt (either the caller's typed value, or the
//                 server-side STT result when refTxt was omitted).
// DELETE /voices/:id → 200 (or 404)
// GET /voices/:id/reference → the original reference wav bytes, for
//                             browser-side review playback.
//
// The reference wav is sent as the raw request body. `refTxt` is optional
// — omit to let the server auto-transcribe via STT; include to override
// with a typed transcription. `filename` is cosmetic (Gradio preserves
// the extension in the returned temp path).

export async function createVoice(wavBlob, { refTxt = null, useXvec = true, filename = 'reference.wav' } = {}) {
  const params = new URLSearchParams({
    use_xvec: useXvec ? 'true' : 'false',
    filename,
  });
  if (refTxt && refTxt.trim()) {
    params.set('ref_txt', refTxt.trim());
  }
  const r = await fetch(`/voices?${params}`, {
    method: 'POST',
    headers: { 'content-type': 'application/octet-stream' },
    body: wavBlob,
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Absolute URL for the reference wav bytes tied to a voice-prompt file
/// id. Suitable as a `<audio src>` for review playback.
export function voiceReferenceUrl(voiceId) {
  return `/voices/${encodeURIComponent(voiceId)}/reference`;
}

export async function deleteVoice(id) {
  const r = await fetch(`/voices/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

// --- Sample clips (raw audio for SynthMode::Sample layers) ---------------
//
// POST /samples          → { id, filename, size } — raw audio bytes go in
//                          the body, ?filename= carries the display name.
//                          Any codec symphonia can decode (wav / flac /
//                          ogg / mp3) is accepted.
// GET  /samples/:id/audio → the original bytes back for review playback.
// DELETE /samples/:id     → drop the row + on-disk file.
//
// Distinct from /voices — samples never go through Qwen3's save_prompt,
// so they can be servo motors, machine chatter, any non-speech texture.

export async function createSample(blob, { filename = 'sample.wav' } = {}) {
  const params = new URLSearchParams({ filename });
  const r = await fetch(`/samples?${params}`, {
    method: 'POST',
    headers: { 'content-type': 'application/octet-stream' },
    body: blob,
  });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
  return await r.json();
}

/// Absolute URL for a sample's audio bytes. Suitable as an `<audio src>`.
export function sampleAudioUrl(sampleId) {
  return `/samples/${encodeURIComponent(sampleId)}/audio`;
}

export async function deleteSample(id) {
  const r = await fetch(`/samples/${encodeURIComponent(id)}`, { method: 'DELETE' });
  if (!r.ok) {
    const detail = await r.text().catch(() => `HTTP ${r.status}`);
    throw new Error(detail || `HTTP ${r.status}`);
  }
}

export async function createTake(blockId, configs = [], agentId = null, projectId = null) {
  const body = { configs };
  if (agentId) body.agentId = agentId;
  if (projectId) body.projectId = projectId;
  const r = await fetch(`/script/blocks/${encodeURIComponent(blockId)}/takes`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
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
