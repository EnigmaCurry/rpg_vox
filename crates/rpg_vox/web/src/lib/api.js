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
/// in an earlier session (widgetId came from localStorage). 404 means the
/// server-side row or WAV was lost; callers fall back to a fresh render.
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

async function widgetRender(method, path, text, instruct) {
  const r = await fetch(path, {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(instruct ? { text, instruct } : { text }),
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

export const createWidget = (text, instruct = '') =>
  widgetRender('POST', '/widgets', text, instruct);

export const updateWidget = (id, text, instruct = '') =>
  widgetRender('PUT', `/widgets/${encodeURIComponent(id)}`, text, instruct);

export async function deleteWidget(id) {
  const r = await fetch(`/widgets/${encodeURIComponent(id)}`, { method: 'DELETE' });
  const body = await r.json().catch(() => null);
  if (!r.ok || (body && body.ok === false)) {
    throw new Error(body?.error || `HTTP ${r.status}`);
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
