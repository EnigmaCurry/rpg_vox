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

export const say            = (text)          => jsonPost('/say', { text });
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
