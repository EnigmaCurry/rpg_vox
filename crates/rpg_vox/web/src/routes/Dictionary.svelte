<script>
  // Per-project pronunciation proxies for TTS. Each entry maps a source
  // word (as it appears in text) to a phonetic respelling the TTS backend
  // pronounces correctly. Substitution is case-insensitive and whole-word,
  // applied server-side just before synthesis so LLM prompts never see the
  // rewritten spelling.
  //
  // Test playback: pick one of the project's character voices from the
  // dropdown, then hit ▶ on any entry to render + speak that word through
  // the pipewire virtual mic. The server applies the dictionary before
  // synth, so what you hear IS the pronunciation column read back — the
  // fastest way to iterate on a stubborn proper-noun spelling.

  import { onDestroy } from 'svelte';
  import {
    currentProject,
    currentProjectDictionary,
    currentProjectCharacters,
    addDictionaryEntry,
    updateDictionaryEntry,
    removeDictionaryEntry,
  } from '../lib/scenes.svelte.js';
  import {
    createWidget,
    updateWidget,
    playWidget,
    deleteWidget,
    stopPlayback,
  } from '../lib/api.js';

  const project = $derived(currentProject());
  const entries = $derived(currentProjectDictionary());
  const characters = $derived(currentProjectCharacters());

  // Flat list of every (character, profile) pair in the project — a single
  // dropdown is friendlier than two cascaded pickers for a page whose main
  // action is quick pronunciation auditioning.
  const voiceOptions = $derived(
    characters.flatMap((c) =>
      (c.voiceProfiles ?? []).map((p) => ({
        key: `${c.id}:${p.id}`,
        characterId: c.id,
        profileId: p.id,
        label: `${c.name} — ${p.name}`,
      })),
    ),
  );

  let selectedVoiceKey = $state('');
  const selectedVoice = $derived(
    voiceOptions.find((v) => v.key === selectedVoiceKey) ?? null,
  );

  // Auto-select the first voice once options materialize (post-hydrate) so
  // the play buttons are usable without an explicit picker click. Also
  // re-runs when the project changes and the old key is no longer valid.
  $effect(() => {
    if (voiceOptions.length === 0) {
      selectedVoiceKey = '';
      return;
    }
    if (!voiceOptions.some((v) => v.key === selectedVoiceKey)) {
      selectedVoiceKey = voiceOptions[0].key;
    }
  });

  let newWord = $state('');
  let newPronunciation = $state('');

  // Two-click delete confirm keyed by entry id.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  // A single reusable widget backs every entry's test playback — each ▶
  // click reuses the same server row via PUT so we don't leak N clips per
  // page visit. Cleaned up on navigate-away.
  let widgetId = null;
  let playAbort = null;
  let playingEntryId = $state(null);
  let playError = $state('');

  onDestroy(() => {
    if (playAbort) {
      try { playAbort.abort(); } catch {}
      playAbort = null;
    }
    if (widgetId) {
      const id = widgetId;
      widgetId = null;
      deleteWidget(id).catch(() => {});
    }
  });

  function onAdd(ev) {
    ev.preventDefault();
    if (!project) return;
    const word = newWord.trim();
    const pronunciation = newPronunciation.trim();
    if (!word || !pronunciation) return;
    addDictionaryEntry(project.id, word, pronunciation);
    newWord = '';
    newPronunciation = '';
  }

  function onWordInput(entryId, ev) {
    updateDictionaryEntry(project.id, entryId, { word: ev.currentTarget.value });
  }
  function onPronunciationInput(entryId, ev) {
    updateDictionaryEntry(project.id, entryId, { pronunciation: ev.currentTarget.value });
  }

  function armDelete(id) {
    if (deleteArmedFor === id) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      removeDictionaryEntry(project.id, id);
      return;
    }
    deleteArmedFor = id;
    if (deleteArmTimer) clearTimeout(deleteArmTimer);
    deleteArmTimer = setTimeout(() => {
      deleteArmedFor = null;
      deleteArmTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  function showPlayError(msg) {
    playError = msg;
    setTimeout(() => { playError = ''; }, 3500);
  }

  async function onPlayEntry(entry) {
    // If we're currently playing this same entry, treat the click as Stop.
    if (playingEntryId === entry.id) {
      onStop();
      return;
    }
    const word = (entry.word ?? '').trim();
    if (!word) { showPlayError('word is empty'); return; }
    if (!selectedVoice) { showPlayError('pick a voice first'); return; }
    if (!project) return;

    // Abort any prior playback (this entry or another) before kicking off
    // the next one — the shared widget is single-slot.
    if (playAbort) {
      try { playAbort.abort(); } catch {}
      playAbort = null;
    }
    stopPlayback().catch(() => {});
    playError = '';
    playingEntryId = entry.id;

    const ac = new AbortController();
    playAbort = ac;
    try {
      const opts = {
        signal: ac.signal,
        characterId: selectedVoice.characterId,
        profileId: selectedVoice.profileId,
        projectId: project.id,
      };
      let result;
      try {
        result = widgetId
          ? await updateWidget(widgetId, word, [], opts)
          : await createWidget(word, [], opts);
      } catch (e) {
        if (e?.name === 'AbortError') throw e;
        // Handle server-side drop of the row (e.g. widget cascade from a
        // character delete) by falling back to a fresh create.
        if (widgetId && /no widget with id/i.test(e.message || '')) {
          widgetId = null;
          result = await createWidget(word, [], opts);
        } else {
          throw e;
        }
      }
      if (result?.id) widgetId = result.id;
      if (!widgetId) throw new Error('no widget id returned');
      await playWidget(widgetId, { signal: ac.signal });
    } catch (e) {
      if (e?.name !== 'AbortError') {
        showPlayError(e.message || 'playback failed');
      }
    } finally {
      if (playAbort === ac) playAbort = null;
      if (playingEntryId === entry.id) playingEntryId = null;
    }
  }

  function onStop() {
    if (playAbort) {
      try { playAbort.abort(); } catch {}
      playAbort = null;
    }
    stopPlayback().catch(() => {});
    playingEntryId = null;
  }
</script>

<section class="wrap">
  <header class="head">
    <h1>Dictionary</h1>
    <p class="sub">
      {#if project}
        Pronunciation proxies for <b>{project.name}</b>. Each entry rewrites the
        <em>word</em> to its <em>pronunciation</em> before TTS synthesis. Matching is
        case-insensitive and whole-word only, so <code>Omnisiah</code> matches
        <code>Omnisiah!</code> but not <code>Omnisiahs</code>. LLM prompts are
        unaffected — respellings never reach the language model. Hit ▶ next to
        a word to hear it read by the selected voice.
      {:else}
        Load a project from the <a href="#/projects">Projects</a> tab to manage its dictionary.
      {/if}
    </p>
  </header>

  {#if project}
    <div class="voice-picker">
      <label class="voice-label" for="voice-select">Test voice</label>
      {#if voiceOptions.length === 0}
        <span class="voice-empty">
          Add a <a href="#/characters">character</a> to enable playback.
        </span>
      {:else}
        <select
          id="voice-select"
          bind:value={selectedVoiceKey}
          aria-label="Test voice"
        >
          {#each voiceOptions as opt (opt.key)}
            <option value={opt.key}>{opt.label}</option>
          {/each}
        </select>
      {/if}
    </div>

    <form class="new" onsubmit={onAdd}>
      <input
        type="text"
        placeholder="Word (as written)"
        bind:value={newWord}
        aria-label="Word"
      />
      <span class="arrow" aria-hidden="true">→</span>
      <input
        type="text"
        placeholder="Pronunciation (as spoken)"
        bind:value={newPronunciation}
        aria-label="Pronunciation"
      />
      <button type="submit" disabled={!newWord.trim() || !newPronunciation.trim()}>Add</button>
    </form>

    {#if playError}
      <div class="err">{playError}</div>
    {/if}

    {#if entries.length === 0}
      <div class="empty">No entries yet — add a word above to teach the TTS a pronunciation.</div>
    {:else}
      <ul class="entry-list">
        {#each entries as entry (entry.id)}
          <li class="entry">
            <button
              type="button"
              class="play"
              class:active={playingEntryId === entry.id}
              onclick={() => onPlayEntry(entry)}
              disabled={voiceOptions.length === 0 || !entry.word.trim()}
              title={playingEntryId === entry.id ? 'Stop' : 'Render and play this word with the selected voice'}
              aria-label={playingEntryId === entry.id ? 'Stop playback' : 'Play this word'}
            >
              {#if playingEntryId === entry.id}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                  <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
                </svg>
              {:else}
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                  <path fill="currentColor" d="M8 5v14l11-7z"/>
                </svg>
              {/if}
            </button>
            <input
              class="entry-word"
              type="text"
              value={entry.word}
              placeholder="word"
              oninput={(e) => onWordInput(entry.id, e)}
              aria-label="Word"
            />
            <span class="arrow" aria-hidden="true">→</span>
            <input
              class="entry-pron"
              type="text"
              value={entry.pronunciation}
              placeholder="pronunciation"
              oninput={(e) => onPronunciationInput(entry.id, e)}
              aria-label="Pronunciation"
            />
            <button
              type="button"
              class="del"
              class:armed={deleteArmedFor === entry.id}
              onclick={() => armDelete(entry.id)}
              title={deleteArmedFor === entry.id ? 'Click again to delete' : 'Delete entry'}
              aria-label="Delete entry"
            >{deleteArmedFor === entry.id ? '?' : '×'}</button>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .head h1 {
    margin: 0 0 4px;
    font-size: 20px;
    font-weight: 600;
    color: var(--text);
  }
  .sub {
    margin: 0;
    color: var(--muted);
    font-size: 13px;
    line-height: 1.5;
  }
  .sub code {
    background: rgba(255,255,255,0.06);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 12px;
  }
  .sub a { color: var(--accent); }

  .voice-picker {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .voice-label {
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .voice-picker select {
    flex: 1;
    min-width: 220px;
    max-width: 420px;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
  }
  .voice-picker select:focus { outline: none; border-color: var(--accent); }
  .voice-empty {
    font-size: 12px;
    color: var(--muted);
    font-style: italic;
  }
  .voice-empty a { color: var(--accent); }

  .new {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto;
    gap: 8px;
    align-items: center;
  }
  .new input {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
  }
  .new input:focus { outline: none; border-color: var(--accent); }
  .new button {
    padding: 8px 14px;
    background: rgba(122,162,255,0.14);
    color: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 6px;
    font-size: 13px;
    font-family: inherit;
    cursor: pointer;
  }
  .new button:hover:not(:disabled) { background: rgba(122,162,255,0.22); }
  .new button:disabled { opacity: 0.4; cursor: not-allowed; }

  .arrow {
    color: var(--muted);
    font-size: 14px;
    padding: 0 2px;
  }

  .empty {
    padding: 24px;
    color: var(--muted);
    font-size: 13px;
    text-align: center;
    border: 1px dashed var(--border);
    border-radius: 8px;
  }
  .err {
    padding: 8px 12px;
    color: var(--err);
    font-size: 12px;
    border: 1px solid var(--err);
    border-radius: 6px;
    background: rgba(255,128,128,0.06);
  }

  .entry-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .entry {
    display: grid;
    grid-template-columns: auto 1fr auto 1fr auto;
    gap: 8px;
    align-items: center;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--panel);
  }
  .entry-word, .entry-pron {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
  }
  .entry-word:focus, .entry-pron:focus { outline: none; border-color: var(--accent); }

  .play {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    background: rgba(122, 162, 255, 0.14);
    color: var(--accent);
    border: 1px solid rgba(122, 162, 255, 0.35);
    border-radius: 6px;
    cursor: pointer;
    flex: 0 0 auto;
  }
  .play:hover:not(:disabled) {
    background: rgba(122, 162, 255, 0.28);
    border-color: var(--accent);
  }
  .play:disabled { opacity: 0.4; cursor: not-allowed; }
  .play.active {
    background: rgba(255, 207, 90, 0.18);
    color: #ffcf5a;
    border-color: rgba(255, 207, 90, 0.5);
  }

  .del {
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 10px;
    font-size: 13px;
    font-family: inherit;
    cursor: pointer;
    line-height: 1;
  }
  .del:hover { color: var(--err); border-color: var(--err); }
  .del.armed {
    color: var(--err);
    border-color: var(--err);
    background: rgba(255,128,128,0.10);
  }
</style>
