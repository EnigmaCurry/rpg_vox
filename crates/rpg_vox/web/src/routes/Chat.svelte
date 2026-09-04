<script>
  import { onMount } from 'svelte';
  import { chatHistory, reloadChatHistory } from '../lib/stores.js';
  import { sendChat, resetChat } from '../lib/api.js';

  let input = $state('');
  let speak = $state(true);
  let sending = $state(false);
  let status = $state({ text: '', kind: '' });
  let logEl;
  let inputEl;

  // Load history the first time the route mounts. Store persists across
  // subsequent mounts so we don't refetch on every visit.
  let loaded = false;
  onMount(async () => {
    if (!loaded) {
      loaded = true;
      try { await reloadChatHistory(); }
      catch (e) { status = { text: `history error: ${e.message}`, kind: 'err' }; }
    }
    scrollBottom();
    inputEl?.focus();
  });

  $effect(() => {
    // Any change to history: scroll to newest
    void $chatHistory;
    scrollBottom();
  });

  function scrollBottom() {
    if (!logEl) return;
    requestAnimationFrame(() => { logEl.scrollTop = logEl.scrollHeight; });
  }

  async function send() {
    const t = input.trim();
    if (!t) return;
    sending = true;
    chatHistory.update((h) => [
      ...h,
      { role: 'user', content: t },
      { role: 'assistant', content: '…', pending: true },
    ]);
    input = '';
    status = { text: 'thinking…', kind: '' };
    try {
      const { ok, body } = await sendChat(t, speak);
      chatHistory.update((h) => {
        const copy = [...h];
        const idx = copy.findIndex((m) => m.pending);
        if (idx < 0) return copy;
        if (ok) copy[idx] = { role: 'assistant', content: body?.reply || '' };
        else    copy.splice(idx, 1);
        return copy;
      });
      status = ok
        ? { text: body?.spoken ? 'reply queued to mic' : 'reply received', kind: 'ok' }
        : { text: `error: ${body?.error || 'unknown'}`, kind: 'err' };
    } catch (e) {
      chatHistory.update((h) => h.filter((m) => !m.pending));
      status = { text: `network error: ${e.message}`, kind: 'err' };
    } finally {
      sending = false;
      inputEl?.focus();
    }
  }

  async function clearAll() {
    if (!confirm('Clear chat history?')) return;
    try {
      await resetChat();
      chatHistory.set([]);
      status = { text: 'history cleared', kind: 'ok' };
    } catch (e) {
      status = { text: `clear failed: ${e.message}`, kind: 'err' };
    }
  }

  function onKey(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }
</script>

<div class="chat">
  <div class="chat-log" bind:this={logEl}>
    {#if $chatHistory.length === 0}
      <div class="empty">no messages yet</div>
    {:else}
      {#each $chatHistory as msg, i (i)}
        <div class="chat-msg {msg.role}" class:pending={msg.pending}>{msg.content}</div>
      {/each}
    {/if}
  </div>

  <textarea
    bind:this={inputEl}
    bind:value={input}
    onkeydown={onKey}
    placeholder="Message the assistant…"></textarea>

  <div class="row">
    <span class="hint">Enter to send · Shift+Enter for newline</span>
    <div class="actions">
      <label class="hint speak-lbl">
        <input type="checkbox" bind:checked={speak}> Speak replies
      </label>
      <button class="secondary" onclick={clearAll}>Clear</button>
      <button onclick={send} disabled={sending}>Send</button>
    </div>
  </div>

  <div class="status {status.kind}">{status.text}</div>
</div>

<style>
  .chat { display: flex; flex-direction: column; gap: 12px; }

  .chat-log {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 10px;
    height: 50vh;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .chat-msg {
    padding: 8px 12px;
    border-radius: 8px;
    max-width: 85%;
    white-space: pre-wrap;
    word-wrap: break-word;
    font-size: 14px;
  }
  .chat-msg.user {
    background: rgba(122,162,255,0.18);
    align-self: flex-end;
  }
  .chat-msg.assistant {
    background: rgba(255,255,255,0.04);
    border: 1px solid var(--border);
    align-self: flex-start;
  }
  .chat-msg.pending { opacity: 0.55; font-style: italic; }

  .actions { display: flex; gap: 8px; align-items: center; }
  .speak-lbl {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
  }
  .speak-lbl input { width: auto; }
</style>
