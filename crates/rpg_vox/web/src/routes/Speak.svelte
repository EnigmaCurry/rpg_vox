<script>
  import { onMount } from 'svelte';
  import { say } from '../lib/api.js';

  let text = $state('');
  let sending = $state(false);
  let status = $state({ text: '', kind: '' });
  let log = $state([]);
  let textarea;

  onMount(() => { textarea?.focus(); });

  function now() { return new Date().toLocaleTimeString(); }

  function pushLog(entry) {
    log = [entry, ...log].slice(0, 20);
  }

  async function submit() {
    const t = text.trim();
    if (!t) return;
    sending = true;
    status = { text: 'speaking…', kind: '' };
    try {
      const { ok, body } = await say(t);
      if (ok) {
        const frames = body?.frames ?? 0;
        status = { text: `spoken (${frames.toLocaleString()} samples)`, kind: 'ok' };
        pushLog({ ts: now(), text: t, err: false });
        text = '';
      } else {
        const detail = body?.error || 'error';
        status = { text: `error: ${detail}`, kind: 'err' };
        pushLog({ ts: now(), text: `${t} — ${detail}`, err: true });
      }
    } catch (e) {
      status = { text: `network error: ${e.message}`, kind: 'err' };
      pushLog({ ts: now(), text: `${t} — ${e.message}`, err: true });
    } finally {
      sending = false;
      textarea?.focus();
    }
  }

  function onKey(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  }
</script>

<div class="speak">
  <textarea
    bind:this={textarea}
    bind:value={text}
    onkeydown={onKey}
    placeholder="Type what the mic should say…"></textarea>

  <div class="row">
    <span class="hint">Enter to send · Shift+Enter for newline</span>
    <button onclick={submit} disabled={sending}>Say it</button>
  </div>

  <div class="status {status.kind}">{status.text}</div>

  <div class="log">
    {#each log as e, i (i + '-' + e.ts + '-' + e.text)}
      <div class="entry" class:err={e.err}>
        <span class="ts">{e.ts}</span>
        <span>{e.text}</span>
      </div>
    {/each}
  </div>
</div>

<style>
  .speak { display: flex; flex-direction: column; gap: 12px; }
  textarea { min-height: 140px; }
  .log { display: flex; flex-direction: column; gap: 6px; margin-top: 8px; }
  .entry {
    padding: 8px 10px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 13px;
    display: flex;
    gap: 10px;
    align-items: baseline;
  }
  .entry .ts { color: var(--muted); font-variant-numeric: tabular-nums; }
  .entry.err { border-color: var(--err); }
</style>
