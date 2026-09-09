<script>
  // Script page: chat-style transcript with a system prompt that instructs
  // the LLM to emit `<speak>…</speak>` blocks for spoken lines. Assistant
  // turns render as markdown, with each speech block replaced inline by a
  // SpeechInline widget (per-take play buttons + a ⟳ to render another
  // take). Prompt input is sticky-bottom; the transcript scrolls above it.

  import { onMount, tick } from 'svelte';
  import { script, reloadScript } from '../lib/stores.js';
  import { sendToScript, resetScript } from '../lib/api.js';
  import ScriptAssistantContent from '../components/ScriptAssistantContent.svelte';

  let input = $state('');
  let sending = $state(false);
  let error = $state('');
  let logEl;
  let inputEl;
  // Turn id we just posted — used to gate autoRender on SpeechInlines so
  // only the newest assistant turn's blocks kick off TTS on mount. Past
  // turns rehydrate silently.
  let latestAssistantId = $state(null);

  // User toggle: when true (default), speech blocks in the freshest
  // assistant turn automatically synth their first take on arrival. When
  // false, the block sits with an empty ▶ button until the user clicks it.
  // Persisted per-browser so the choice sticks across reloads.
  const AUTO_RENDER_KEY = 'rpg_vox.script.auto_render';
  function loadAutoRender() {
    try {
      const v = localStorage.getItem(AUTO_RENDER_KEY);
      return v === null ? true : v === '1';
    } catch {
      return true;
    }
  }
  let autoRender = $state(loadAutoRender());
  $effect(() => {
    try { localStorage.setItem(AUTO_RENDER_KEY, autoRender ? '1' : '0'); } catch {}
  });

  let loaded = false;
  onMount(async () => {
    if (!loaded) {
      loaded = true;
      try {
        await reloadScript();
      } catch (e) {
        error = `history error: ${e.message}`;
      }
    }
    scrollBottom();
    inputEl?.focus();
  });

  $effect(() => {
    // Any store change: re-scroll to the newest turn.
    void $script;
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
    error = '';
    // Optimistic user turn so the transcript reflects the send immediately
    // — the server-side row lands under the `ord` we don't know yet, so we
    // stash a placeholder id and reconcile on response.
    script.update((s) => {
      const base = s || { id: 'default', turns: [] };
      return {
        ...base,
        turns: [
          ...base.turns,
          { id: '__pending', ord: base.turns.length, role: 'user', content: t, blocks: [] },
          { id: '__thinking', ord: base.turns.length + 1, role: 'assistant', content: '…', blocks: [], pending: true },
        ],
      };
    });
    input = '';
    await tick();
    scrollBottom();
    try {
      const { user_turn, assistant_turn } = await sendToScript(t);
      script.update((s) => {
        const base = s || { id: 'default', turns: [] };
        const turns = base.turns.filter(
          (x) => x.id !== '__pending' && x.id !== '__thinking',
        );
        turns.push(user_turn, assistant_turn);
        return { ...base, turns };
      });
      latestAssistantId = assistant_turn.id;
    } catch (e) {
      // Drop the optimistic placeholders so the user can retry cleanly.
      script.update((s) => {
        if (!s) return s;
        return {
          ...s,
          turns: s.turns.filter((x) => x.id !== '__pending' && x.id !== '__thinking'),
        };
      });
      error = `send: ${e.message || e}`;
    } finally {
      sending = false;
      inputEl?.focus();
    }
  }

  async function clearAll() {
    if (!confirm('Clear the script?')) return;
    try {
      await resetScript();
      script.set({ id: 'default', turns: [] });
      latestAssistantId = null;
    } catch (e) {
      error = `clear failed: ${e.message}`;
    }
  }

  function onKey(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  /// Merge a block-level mutation (add take, select, delete) back into the
  /// script store so a re-render sees the updated take list. Immutable
  /// update: fresh objects on the way down so Svelte notices.
  function applyBlockChange(updated) {
    script.update((s) => {
      if (!s) return s;
      const turns = s.turns.map((t) => {
        if (!Array.isArray(t.blocks) || t.blocks.length === 0) return t;
        const idx = t.blocks.findIndex((b) => b.id === updated.id);
        if (idx < 0) return t;
        const nextBlocks = t.blocks.slice();
        nextBlocks[idx] = updated;
        return { ...t, blocks: nextBlocks };
      });
      return { ...s, turns };
    });
  }

  const turns = $derived($script?.turns ?? []);
</script>

<div class="script">
  <div class="log" bind:this={logEl}>
    {#if turns.length === 0}
      <div class="empty">No turns yet. Say something to start the scene.</div>
    {:else}
      {#each turns as t (t.id)}
        <div class="turn {t.role}" class:pending={t.pending}>
          {#if t.role === 'assistant' && !t.pending}
            <ScriptAssistantContent
              content={t.content}
              blocks={t.blocks}
              onblockchange={applyBlockChange}
              autoRender={autoRender && t.id === latestAssistantId}
            />
          {:else}
            <span class="plain">{t.content}</span>
          {/if}
        </div>
      {/each}
    {/if}
  </div>

  <div class="input-bar">
    <textarea
      bind:this={inputEl}
      bind:value={input}
      onkeydown={onKey}
      placeholder="Speak to the scene… (Enter to send · Shift+Enter for newline)"
      rows="2"
    ></textarea>
    <div class="input-actions">
      <label class="auto-render" title="When on, speech blocks in a new reply immediately render their first take. When off, each block waits for you to click ▶.">
        <input type="checkbox" bind:checked={autoRender} />
        <span>auto-render</span>
      </label>
      <button class="secondary" onclick={clearAll} title="Clear the whole script">Clear</button>
      <button onclick={send} disabled={sending || !input.trim()}>{sending ? 'Sending…' : 'Send'}</button>
    </div>
    {#if error}
      <div class="err">{error}</div>
    {/if}
  </div>
</div>

<style>
  .script {
    display: flex;
    flex-direction: column;
    /* Full viewport minus the sticky menubar height (roughly 44px in the
       current design). Locks the input to the bottom of the visible area
       while the transcript scrolls above it. */
    height: calc(100vh - 52px);
    max-width: 900px;
    margin: 0 auto;
    padding: 12px 16px 0;
    gap: 10px;
  }

  .log {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 12px 8px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
  }
  .empty {
    text-align: center;
    color: var(--muted);
    font-style: italic;
    padding: 40px 0;
  }

  .turn {
    padding: 10px 14px;
    border-radius: 10px;
    max-width: 95%;
    word-wrap: break-word;
    line-height: 1.55;
    font-size: 14px;
  }
  .turn.user {
    background: rgba(122, 162, 255, 0.16);
    align-self: flex-end;
    max-width: 80%;
  }
  .turn.assistant {
    background: rgba(255, 255, 255, 0.03);
    border: 1px solid var(--border);
    align-self: flex-start;
  }
  .turn.pending {
    opacity: 0.6;
    font-style: italic;
  }
  .plain {
    white-space: pre-wrap;
  }

  .input-bar {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 6px 14px;
    background: var(--panel);
    border-top: 1px solid var(--border);
    /* Sticky-ish visual anchoring — the flex height above already pins the
       bar to the viewport bottom; this just visually separates it from the
       log. */
    position: sticky;
    bottom: 0;
  }
  textarea {
    width: 100%;
    resize: vertical;
    min-height: 44px;
    max-height: 30vh;
    font-family: inherit;
    font-size: 14px;
    padding: 8px 10px;
    background: rgba(0, 0, 0, 0.28);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
  }
  textarea:focus { outline: none; border-color: var(--accent); }

  .input-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    align-items: center;
  }
  /* Push the auto-render toggle to the LEFT edge of the row so it visually
     groups with the input above it, leaving Clear/Send on the right. */
  .auto-render {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
    cursor: pointer;
    user-select: none;
    margin-right: auto;
  }
  .auto-render input { width: auto; margin: 0; cursor: pointer; }
  .err {
    color: var(--err);
    font-size: 12px;
    padding: 2px 4px;
  }
</style>
