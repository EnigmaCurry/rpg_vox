<script>
  // Script page: chat-style transcript with a system prompt that instructs
  // the LLM to emit `<speak>…</speak>` blocks for spoken lines. Assistant
  // turns render as markdown, with each speech block replaced inline by a
  // SpeechInline widget (per-take play buttons + a ⟳ to render another
  // take). Prompt input is sticky-bottom; the transcript scrolls above it.

  import { onDestroy, onMount, tick } from 'svelte';
  import {
    activeClip,
    agents,
    graph,
    reloadAgents,
    reloadGraph,
    reloadScript,
    reloadScripts,
    script,
    scripts,
  } from '../lib/stores.js';
  import {
    cancelRecording,
    createAgent,
    createScript,
    deleteWidget,
    fetchWidget,
    generateScriptTitle,
    playWidget,
    resetScript,
    sendAssistantReply,
    sendUserTurn,
    startRecording,
    stopPlayback,
    stopRecording,
    updateAgent,
  } from '../lib/api.js';
  import { navigate, route } from '../lib/router.js';
  import { scrollClipIntoView } from '../lib/scroll.js';
  import { currentProjectCharacters, scenesState } from '../lib/scenes.svelte.js';
  import ScriptAssistantContent from '../components/ScriptAssistantContent.svelte';
  import ScriptSidebar from '../components/ScriptSidebar.svelte';

  // --- Multi-script routing --------------------------------------------
  //
  // Hash path formats we accept:
  //   /script          → restore last-loaded from localStorage or land on
  //                      the most recent script.
  //   /script/:id      → deep link into a specific script.
  //   /chat, /chat/:id → same as above (legacy).
  //
  // On selection change we update the hash so the URL becomes a shareable
  // deep link for the current script. Local storage remembers the last
  // loaded id so returning to /script re-opens it.
  const SCRIPT_ID_KEY = 'rpg_vox.script.current_id';
  function stickyScriptId() {
    try { return localStorage.getItem(SCRIPT_ID_KEY); }
    catch { return null; }
  }
  function saveStickyScriptId(id) {
    try {
      if (id) localStorage.setItem(SCRIPT_ID_KEY, id);
      else    localStorage.removeItem(SCRIPT_ID_KEY);
    } catch {}
  }
  /// Parse the script id (if any) out of the current hash route.
  function scriptIdFromRoute(r) {
    const m = /^\/(?:script|chat)\/(.+)$/.exec(r || '');
    return m ? decodeURIComponent(m[1]) : null;
  }

  let currentScriptId = $state(scriptIdFromRoute($route));
  // React to hash changes so back/forward navigation loads the right script.
  $effect(() => {
    const idFromRoute = scriptIdFromRoute($route);
    if (idFromRoute && idFromRoute !== currentScriptId) {
      currentScriptId = idFromRoute;
    }
  });
  // Persist selection whenever it changes.
  $effect(() => {
    if (currentScriptId) saveStickyScriptId(currentScriptId);
  });

  // Responsive sidebar. `narrow` tracks viewport width via matchMedia;
  // when narrow, the sidebar is hidden by default and re-openable via the
  // top-left hamburger. On wider screens the sidebar is always visible
  // and the hamburger doesn't render.
  const NARROW_MQ = '(max-width: 720px)';
  let narrow = $state(false);
  let sidebarOpen = $state(true);
  $effect(() => {
    if (typeof window === 'undefined') return;
    const mq = window.matchMedia(NARROW_MQ);
    const sync = () => {
      narrow = mq.matches;
      if (!mq.matches) sidebarOpen = true;
      else             sidebarOpen = false;
    };
    sync();
    mq.addEventListener('change', sync);
    return () => mq.removeEventListener('change', sync);
  });

  let input = $state('');
  let sending = $state(false);
  let error = $state('');
  let logEl = $state(null);
  // Textarea is conditionally rendered (hidden while recording), so
  // bind:this re-fires whenever we swap in/out — reactive so those
  // updates stay tracked.
  let inputEl = $state(null);

  // Voice-memo recording state. The three states the input bar cycles
  // through:
  //   1. idle-empty (no text, no widget) — show Record button.
  //   2. idle-composed (has text OR attached widget) — show Send + Clear.
  //   3. recording (session open) — hide textarea, show timer + Stop.
  //
  // Recording flow: click Record → POST /widgets/record → server buffers
  // the vox tap → click Stop → POST /widgets/record/:sid/stop → server
  // returns a fresh widgetId + STT transcript. Transcript drops into the
  // textarea for editing; the widgetId stays attached until the user
  // either sends (widgetId ships to /script) or clears (widget is deleted).
  let recordingSessionId = $state(null);
  let recordingStartedAt = 0;
  let recordingElapsedMs = $state(0);
  let recordingRafId = 0;
  // Widget id from a completed recording that hasn't been sent yet. When
  // set, Send attaches it to the /script POST; Clear deletes it server-side.
  let attachedWidgetId = $state(null);

  const isRecording = $derived(recordingSessionId !== null);
  // "Composed" = the input bar has something to send (typed text OR an
  // attached voice memo). Governs Record vs Send/Clear visibility.
  const isComposed = $derived(input.trim().length > 0 || attachedWidgetId !== null);

  // The vox sink is our capture path — /widgets/record subscribes to its
  // PCM tap. If no source (mic, app stream, etc.) is currently routed
  // into the vox sink, recording would just capture silence, so we grey
  // the button out with a hint explaining why.
  const voxSources = $derived(
    ($graph?.sources ?? []).filter((s) => s.routed_to === 'vox')
  );
  const hasVoxSource = $derived(voxSources.length > 0);
  // Turn id we just posted — used to gate autoRender on SpeechInlines so
  // only the newest assistant turn's blocks kick off TTS on mount. Past
  // turns rehydrate silently.
  let latestAssistantId = $state(null);

  // Autoplay sequence: after a fresh assistant reply lands (with auto-render
  // on), we walk the reply's blocks in reading order, playing each one as
  // soon as its take is ready. `autoplayOrd` is the ord of the block that
  // should currently play; each SpeechInline whose ord matches consumes
  // it, plays its selected take, then calls `advanceAutoplay` so the next
  // block takes over. Any manual click on any block cancels the sequence
  // (`autoplayTurnId = null`).
  let autoplayTurnId = $state(null);
  let autoplayOrd = $state(null);

  function advanceAutoplay(_blockId) {
    if (autoplayTurnId === null) return;
    autoplayOrd = (autoplayOrd ?? 0) + 1;
  }
  function cancelAutoplay() {
    autoplayTurnId = null;
    autoplayOrd = null;
  }

  // "Play All" walks the entire conversation from the top: each user turn
  // that has an attached voice memo, and each assistant turn's selected
  // takes in reading order. Snapshotted into `playQueue` at start time so
  // skip-forward / skip-backward can jump within a stable index space
  // even if the underlying `turns` mutate mid-playback.
  let isPlayingAll = $state(false);
  let playQueue = $state([]);  // { widget_id }[]
  let playIndex = $state(0);   // 0-based current clip
  // Not $state: internal-only refs used to break out of the current
  // playWidget promise when the user hits skip / stop.
  let clipAbortController = null;
  let externalCancel = false;
  // Set by skip handlers so the loop knows which way to move after the
  // in-flight fetch bails out. Null means "advance forward on natural end".
  let skipDirection = null;

  /// Snapshot the current transcript into a flat play queue of widget ids
  /// (in reading order). Empty entries (blocks without takes, orphan turns)
  /// are dropped so the index space matches what the user visually sees.
  /// Each entry carries a `durationMs` best-guess (0 = "unknown — fetch"),
  /// which the playback loop feeds into activeClip so the progress bar
  /// on each pill can animate over the correct wall-clock span.
  function buildPlayQueue(turnList) {
    const q = [];
    for (const t of turnList) {
      if (t.role === 'user' && t.widget_id) {
        q.push({ widget_id: t.widget_id, durationMs: 0 });
      } else if (t.role === 'assistant' && Array.isArray(t.blocks)) {
        for (const b of t.blocks) {
          const takes = b.takes ?? [];
          if (takes.length === 0) continue;
          const chosen = takes.find((x) => x.ord === b.selected_take)
            ?? takes[takes.length - 1];
          if (chosen?.widget_id) {
            q.push({
              widget_id: chosen.widget_id,
              durationMs: chosen.duration_ms || 0,
            });
          }
        }
      }
    }
    return q;
  }

  /// Any user-driven interaction that should abort in-flight sequencing.
  /// Called both from ScriptAssistantContent's onAutoInterrupt callback
  /// and locally when the user hits Clear / plays a user widget.
  function cancelAllSequences() {
    cancelAutoplay();
    if (isPlayingAll) cancelPlayAll();
  }

  function cancelPlayAll() {
    externalCancel = true;
    if (clipAbortController) {
      try { clipAbortController.abort(); } catch {}
    }
    stopPlayback().catch(() => {});
  }

  /// Toggle Play All: if a queue is already running, stop it; otherwise
  /// kick off a fresh top-of-conversation walk.
  function togglePlayAll() {
    if (isPlayingAll) cancelPlayAll();
    else playAll();
  }

  /// Skip to the next clip in the queue. Aborts the current /widgets/:id/say
  /// fetch and drains the pipewire mic so the following clip starts fresh.
  ///
  /// At the end of the queue, forward RE-TRIGGERS the last clip instead
  /// of exiting playback. Reason: exiting swaps the [⏮][Stop N/N][⏭]
  /// compound back to a single [Play All] button, which shifts Clear/Send
  /// to the left — right on top of where the user's cursor already is
  /// from clicking Skip. Staying in playback keeps the layout stable, and
  /// the intent ("play the last one again") is what an over-eager
  /// Skip-Forward tap actually asks for anyway.
  function skipForward() {
    if (!isPlayingAll) return;
    skipDirection = playIndex >= playQueue.length - 1 ? 'replay' : 'forward';
    if (clipAbortController) {
      try { clipAbortController.abort(); } catch {}
    }
    stopPlayback().catch(() => {});
  }

  /// Skip to the previous clip in the queue. At index 0, replays the
  /// current clip from the top (matches typical media-player skip-back
  /// semantics for the first item).
  function skipBackward() {
    if (!isPlayingAll) return;
    skipDirection = 'backward';
    if (clipAbortController) {
      try { clipAbortController.abort(); } catch {}
    }
    stopPlayback().catch(() => {});
  }

  async function playAll() {
    // Fresh Play All request: cancel any queue currently running so we
    // start from the top with predictable state.
    cancelAllSequences();
    try { await stopPlayback(); } catch {}

    const queue = buildPlayQueue(turns);
    if (queue.length === 0) return;

    playQueue = queue;
    playIndex = 0;
    isPlayingAll = true;
    externalCancel = false;
    error = '';

    try {
      while (playIndex < playQueue.length && !externalCancel) {
        const clip = playQueue[playIndex];
        clipAbortController = new AbortController();
        skipDirection = null;
        // User memos don't come with a duration in the timeline blob (the
        // server only echoes take metadata, not attached widgets), so fill
        // it in on demand the first time we play the clip. Guarded catch
        // so an unresolvable widget just falls back to a 1s estimate.
        let durationMs = clip.durationMs;
        if (!durationMs) {
          try {
            const info = await fetchWidget(clip.widget_id);
            durationMs = info.durationMs || 0;
            clip.durationMs = durationMs;
          } catch {}
        }
        activeClip.set({
          widgetId: clip.widget_id,
          durationMs: durationMs || 1000,
          startedAt: performance.now(),
        });
        try {
          await playWidget(clip.widget_id, { signal: clipAbortController.signal });
        } catch (e) {
          // AbortError is expected from skip / cancel; anything else is a
          // real failure we log and move past rather than derailing the
          // whole queue.
          if (e?.name !== 'AbortError') {
            console.warn('play-all clip failed', e);
          }
        }
        // Only clear if we're still the active clip — the next iteration
        // will publish its own activeClip immediately, so this avoids a
        // one-frame gap where progress bars flash to zero.
        activeClip.update((cur) => (cur?.widgetId === clip.widget_id ? null : cur));
        if (externalCancel) break;
        if (skipDirection === 'backward') {
          playIndex = Math.max(0, playIndex - 1);
        } else if (skipDirection === 'replay') {
          // Skip-forward pressed at end-of-queue → repeat the current
          // clip. Same index, next loop iteration re-fetches and re-plays.
        } else {
          // 'forward' skip AND natural end-of-clip both advance by one.
          playIndex += 1;
        }
      }
    } finally {
      isPlayingAll = false;
      playQueue = [];
      playIndex = 0;
      clipAbortController = null;
      externalCancel = false;
      skipDirection = null;
      activeClip.set(null);
    }
  }

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

  // --- Agent picker + editor ------------------------------------------------
  //
  // Agents are named system-prompt profiles stored on the server. The picker
  // in the input bar chooses which one runs on the next /script send; the
  // "edit agent" toggle reveals a 50vh editor panel above the transcript
  // for autosaving changes.
  //
  // Default agent id is hardcoded to match the server's DEFAULT_AGENT_ID
  // constant. localStorage remembers the last selection so a new session
  // opens with the same agent — user can still pick Default manually at
  // any time.
  const DEFAULT_AGENT_ID = 'default';
  const AGENT_KEY = 'rpg_vox.script.agent_id';
  const EDIT_AGENT_KEY = 'rpg_vox.script.edit_agent';
  // Sentinel select value that opens the "create a new agent" prompt when
  // chosen. Distinct from any real agent id so we can't collide.
  const CREATE_SENTINEL = '__create__';

  function loadStickyAgentId() {
    try { return localStorage.getItem(AGENT_KEY) || DEFAULT_AGENT_ID; }
    catch { return DEFAULT_AGENT_ID; }
  }
  function loadEditAgent() {
    try { return localStorage.getItem(EDIT_AGENT_KEY) === '1'; }
    catch { return false; }
  }

  let selectedAgentId = $state(loadStickyAgentId());
  let editAgent = $state(loadEditAgent());
  // Local editor draft — synced from the store, autosaved back after a
  // debounce so we don't PUT on every keystroke.
  let promptDraft = $state('');
  let promptDirty = $state(false);
  let saveTimer = 0;
  let saving = $state(false);
  let saveError = $state('');

  $effect(() => {
    try { localStorage.setItem(AGENT_KEY, selectedAgentId); } catch {}
  });
  $effect(() => {
    try { localStorage.setItem(EDIT_AGENT_KEY, editAgent ? '1' : '0'); } catch {}
  });

  const activeAgent = $derived(
    ($agents ?? []).find((a) => a.id === selectedAgentId) ?? null
  );
  const activeAgentReadOnly = $derived(activeAgent?.read_only ?? true);

  // Sync draft from the server-side agent when the SELECTION changes.
  // Guarded by promptDirty so an in-flight edit doesn't get clobbered by
  // a stale reactive read of the agents store.
  let lastLoadedId = null;
  $effect(() => {
    if (!activeAgent) return;
    if (activeAgent.id === lastLoadedId) return;
    lastLoadedId = activeAgent.id;
    promptDraft = activeAgent.system_prompt || '';
    promptDirty = false;
    saveError = '';
  });

  function onPromptInput() {
    if (activeAgentReadOnly) return;
    promptDirty = true;
    if (saveTimer) clearTimeout(saveTimer);
    // 600 ms is short enough that Cmd-Tabbing away saves what you had, but
    // long enough that we don't hammer PUT during fast typing.
    saveTimer = setTimeout(savePromptDraft, 600);
  }

  async function savePromptDraft() {
    if (!activeAgent || activeAgentReadOnly) return;
    if (!promptDirty) return;
    saving = true;
    saveError = '';
    const id = activeAgent.id;
    const draft = promptDraft;
    try {
      await updateAgent(id, { systemPrompt: draft });
      // Merge the saved draft back into the store so activeAgent reflects
      // the just-saved value (avoids the sync effect above stomping the
      // draft on the next tick).
      agents.update((list) =>
        (list || []).map((a) => (a.id === id ? { ...a, system_prompt: draft } : a))
      );
      promptDirty = false;
    } catch (e) {
      saveError = `save: ${e.message || e}`;
    } finally {
      saving = false;
    }
  }

  async function onAgentSelectChange(ev) {
    const val = ev.currentTarget.value;
    if (val === CREATE_SENTINEL) {
      // Reset the <select> back to the previous choice IMMEDIATELY so the
      // sentinel doesn't stick if the user cancels the prompt.
      ev.currentTarget.value = selectedAgentId;
      await createNewAgentFlow();
      return;
    }
    // If we were editing an unsaved change on the previous agent, flush it
    // before switching so the change doesn't get lost.
    if (promptDirty) {
      if (saveTimer) { clearTimeout(saveTimer); saveTimer = 0; }
      await savePromptDraft();
    }
    selectedAgentId = val;
  }

  async function createNewAgentFlow() {
    // eslint-disable-next-line no-alert
    const name = window.prompt('Name for the new agent:');
    if (name === null) return;  // Cancel
    const trimmed = name.trim();
    if (!trimmed) return;
    const pid = scenesState.selectedProjectId;
    if (!pid) {
      saveError = 'create: pick a project first (Projects tab)';
      return;
    }
    try {
      const created = await createAgent(trimmed, pid);
      // Refresh + select the new one so the editor lights up on it.
      await reloadAgents(pid);
      selectedAgentId = created.id;
    } catch (e) {
      saveError = `create: ${e.message || e}`;
    }
  }

  // Agents are project-scoped (except the built-in Default, which shows in
  // every project). Reload whenever the user switches projects so the
  // dropdown reflects the current roster. Explicit read of scenesState
  // makes Svelte's reactivity graph track it.
  $effect(() => {
    const pid = scenesState.selectedProjectId;
    reloadAgents(pid).catch(() => {});
  });

  // Characters available to fill the agent's voice slots — same roster the
  // Characters tab shows for the current project.
  const projectCharacters = $derived(currentProjectCharacters());

  // If the previously-selected agent isn't in the newly-loaded list (project
  // switch, or the agent was deleted elsewhere), snap back to Default so
  // the picker never shows a stale label.
  $effect(() => {
    const list = $agents ?? [];
    if (list.length === 0) return;
    if (!list.some((a) => a.id === selectedAgentId)) {
      selectedAgentId = DEFAULT_AGENT_ID;
    }
  });

  /// Persist a voice slot change back to the agent + optimistically update
  /// the local store so the <select> reflects the choice immediately.
  /// `field` is one of 'voiceUser' / 'voiceNarrator' / 'voiceCharacter';
  /// empty string clears the slot back to the DSP-preset fallback.
  async function onVoiceSlotChange(field, ev) {
    if (!activeAgent || activeAgentReadOnly) return;
    const raw = ev.currentTarget.value;
    const characterId = raw === '' ? null : raw;
    const snakeField = {
      voiceUser: 'voice_user',
      voiceNarrator: 'voice_narrator',
      voiceCharacter: 'voice_character',
    }[field];
    const id = activeAgent.id;
    saveError = '';
    try {
      await updateAgent(id, { [field]: characterId });
      agents.update((list) =>
        (list || []).map((a) => (a.id === id ? { ...a, [snakeField]: characterId } : a))
      );
    } catch (e) {
      saveError = `voice: ${e.message || e}`;
    }
  }

  // Poll the pipewire graph so Record enables/disables in near-real-time
  // when the user routes a mic or app stream into the vox sink. 3s matches
  // the Settings-page poll; graph fetches are cheap.
  let graphTimer = null;
  onMount(async () => {
    // Kick off collateral fetches first so they parallelize with the
    // script load. (reloadAgents runs reactively via the project-switch
    // effect above, so no explicit kickoff here.)
    reloadGraph().catch(() => {});
    graphTimer = setInterval(() => reloadGraph().catch(() => {}), 3000);

    // Resolve which script to load:
    //  1. Explicit id in the URL hash wins.
    //  2. Sticky localStorage from the last visit is next.
    //  3. Fall back to the most-recently-updated script (from the list).
    //  4. If the store is entirely empty, create one and load it.
    try {
      await reloadScripts();
    } catch (e) {
      error = `scripts error: ${e.message}`;
    }
    const list = $scripts ?? [];
    let idToLoad = scriptIdFromRoute($route) || stickyScriptId();
    if (idToLoad && !list.some((s) => s.id === idToLoad)) idToLoad = null;
    if (!idToLoad && list.length > 0) idToLoad = list[0].id;
    if (!idToLoad) {
      try {
        const created = await createScript();
        scripts.update((l) => [created, ...(l || [])]);
        idToLoad = created.id;
      } catch (e) {
        error = `create: ${e.message}`;
        return;
      }
    }
    // If the URL didn't already have the id, add it so refreshes stick to
    // this script and it can be linked to.
    if (!scriptIdFromRoute($route)) {
      navigate(`/script/${idToLoad}`);
    }
    currentScriptId = idToLoad;
    await loadScriptById(idToLoad);
    scrollBottom();
    inputEl?.focus();
  });

  /// Reactively reload the transcript whenever the selected script id
  /// changes (URL nav, sidebar click, etc). Idempotent — reloading the
  /// same id is a no-op-ish (fresh /scripts/:id fetch) so the ordering
  /// with `onMount` above is safe.
  let lastLoadedScriptId = $state(null);
  $effect(() => {
    if (!currentScriptId) return;
    if (currentScriptId === lastLoadedScriptId) return;
    lastLoadedScriptId = currentScriptId;
    loadScriptById(currentScriptId);
  });

  async function loadScriptById(id) {
    try {
      await reloadScript(id);
      // Update the URL to reflect the just-loaded id (idempotent when the
      // hash already matches).
      if (scriptIdFromRoute($route) !== id) navigate(`/script/${id}`);
      // Clear per-script client state so playing turns from the OLD
      // script don't linger.
      cancelAllSequences();
      latestAssistantId = null;
      attachedWidgetId = null;
      input = '';
    } catch (e) {
      error = `history error: ${e.message}`;
    }
  }

  /// Sidebar callback: user picked a different script row.
  function onSidebarSelect(id) {
    if (id === currentScriptId) return;
    currentScriptId = id;
    // Navigation happens inside the sidebar too, but do it here as well
    // so callers that don't route can still switch scripts.
    navigate(`/script/${id}`);
  }

  $effect(() => {
    // Any store change: re-scroll to the newest turn.
    void $script;
    scrollBottom();
  });

  function scrollBottom() {
    if (!logEl) return;
    requestAnimationFrame(() => { logEl.scrollTop = logEl.scrollHeight; });
  }

  // Best-effort cleanup: if the user navigates away or closes the tab
  // mid-recording, fire the cancel so the server doesn't hold the vox
  // buffer indefinitely. deleteWidget for an attached-but-not-sent memo
  // is also best-effort — worst case it stays as an orphan in the store.
  onDestroy(() => {
    if (recordingRafId) cancelAnimationFrame(recordingRafId);
    if (graphTimer) { clearInterval(graphTimer); graphTimer = null; }
    if (recordingSessionId) {
      const sid = recordingSessionId;
      recordingSessionId = null;
      cancelRecording(sid).catch(() => {});
    }
  });

  function tickRecord() {
    if (!isRecording) return;
    recordingElapsedMs = performance.now() - recordingStartedAt;
    recordingRafId = requestAnimationFrame(tickRecord);
  }

  function formatMMSS(ms) {
    const total = Math.max(0, Math.floor(ms / 1000));
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
  }
  const recordingElapsedLabel = $derived(formatMMSS(recordingElapsedMs));

  // Synchronous guard so a Record button that's wired to BOTH pointerdown
  // (for the "engages before you release the mouse" feel) and click (for
  // keyboard activation via Enter/Space) can't double-fire two /widgets/
  // record sessions on the server. Set true synchronously; cleared once
  // the server hands back a session id (or on failure).
  let startingRecord = false;
  async function startRecord(ev) {
    // Ignore non-primary mouse buttons — right-click shouldn't start a
    // recording session. `button` is undefined on non-mouse events.
    if (ev && ev.button !== undefined && ev.button !== 0) return;
    if (isRecording || startingRecord) return;
    // Belt-and-braces: Safari has historically fired pointerdown on
    // disabled buttons; refuse to start if there's no producer feeding
    // the vox sink or we'd just capture silence.
    if (!hasVoxSource) return;
    startingRecord = true;
    error = '';
    try {
      const { sessionId } = await startRecording({ text: '' });
      recordingSessionId = sessionId;
      recordingStartedAt = performance.now();
      recordingElapsedMs = 0;
      recordingRafId = requestAnimationFrame(tickRecord);
    } catch (e) {
      error = `record: ${e.message || e}`;
    } finally {
      startingRecord = false;
    }
  }

  async function stopRecord() {
    if (!isRecording) return;
    const sid = recordingSessionId;
    recordingSessionId = null;
    if (recordingRafId) { cancelAnimationFrame(recordingRafId); recordingRafId = 0; }
    try {
      const result = await stopRecording(sid);
      // The server returns the newly-created widget id + its STT transcript.
      // We attach both: transcript pre-fills the textarea for editing;
      // widgetId ships with /script POST so the user's turn keeps its audio.
      attachedWidgetId = result.id;
      if (typeof result.transcript === 'string' && result.transcript.length > 0) {
        input = result.transcript;
      }
      // Focus the textarea now that the transcription is in — user is
      // most likely to want to review/edit it before hitting Send.
      await tick();
      inputEl?.focus();
    } catch (e) {
      error = `stop: ${e.message || e}`;
    }
  }

  async function send() {
    const t = input.trim();
    // Voice-memo-only sends are OK (widget attached, transcript empty) —
    // the LLM sees an empty user message but the recording still shows on
    // the timeline for playback. Text-only sends are the common path.
    if (!t && !attachedWidgetId) return;
    sending = true;
    error = '';
    const outgoingWidgetId = attachedWidgetId;
    // Optimistic user turn so the transcript reflects the send immediately
    // — the server-side row lands under the `ord` we don't know yet, so we
    // stash a placeholder id and reconcile on response.
    script.update((s) => {
      const base = s || { id: 'default', turns: [] };
      return {
        ...base,
        turns: [
          ...base.turns,
          { id: '__pending', ord: base.turns.length, role: 'user', content: t, widget_id: outgoingWidgetId, blocks: [] },
          { id: '__thinking', ord: base.turns.length + 1, role: 'assistant', content: '…', blocks: [], pending: true },
        ],
      };
    });
    input = '';
    attachedWidgetId = null;
    await tick();
    scrollBottom();
    // Whether the user provided their own recording. Only GM-proxy turns
    // (i.e. no recording) get the immediate auto-play treatment on arrival
    // — recorded turns are the user's own voice, playing it back at them
    // right after they just spoke would be weird.
    const wasTyped = !outgoingWidgetId;
    try {
      // Phase 1: user turn. Fast — returns as soon as the GM proxy synth
      // (or the trivial no-synth path when a recording is attached) lands
      // in the widget store.
      const scriptId = currentScriptId;
      const wasNamed = (($scripts ?? []).find((s) => s.id === scriptId)?.name ?? '') !== 'New script';
      const { user_turn } = await sendUserTurn(scriptId, t, outgoingWidgetId, selectedAgentId);
      script.update((s) => {
        const base = s || { id: 'default', turns: [] };
        // Rebuild the order deliberately: real turns first, then the
        // just-landed user turn, then keep the __thinking placeholder AT
        // THE END so the animated dots sit below the user's message
        // instead of above it (which is what a naive filter+push would
        // produce, since __thinking was pushed to the array before the
        // user turn arrived).
        const kept = base.turns.filter(
          (x) => x.id !== '__pending' && x.id !== '__thinking',
        );
        kept.push(user_turn);
        kept.push({
          id: '__thinking',
          ord: kept.length,
          role: 'assistant',
          content: '…',
          blocks: [],
          pending: true,
        });
        return { ...base, turns: kept };
      });
      await tick();
      scrollBottom();
      // Kick playback of the GM proxy immediately so the user hears their
      // typed message read aloud while the LLM is still working on the
      // reply. The activeClip gate in SpeechInline keeps the assistant
      // blocks from stepping on this playback when they arrive.
      if (autoRender && wasTyped && user_turn.widget_id) {
        playUserTurnFireAndForget(user_turn.widget_id);
      }
      // Phase 2: LLM reply. Slow — happens in parallel with (2)'s playback.
      const { assistant_turn } = await sendAssistantReply(scriptId, selectedAgentId);
      script.update((s) => {
        const base = s || { id: 'default', turns: [] };
        const turns = base.turns.filter((x) => x.id !== '__thinking');
        turns.push(assistant_turn);
        return { ...base, turns };
      });
      latestAssistantId = assistant_turn.id;
      // Kick autoplay of the assistant blocks. The activeClip gate in
      // SpeechInline holds this until the GM proxy (or user memo) has
      // finished draining, so the two don't step on each other.
      if (autoRender && (assistant_turn.blocks?.length ?? 0) > 0) {
        autoplayTurnId = assistant_turn.id;
        autoplayOrd = 0;
      } else {
        autoplayTurnId = null;
        autoplayOrd = null;
      }
      // First exchange in a fresh "New script" → kick off an LLM title
      // generation. Fire-and-forget: the sidebar reload picks up the new
      // name on completion.
      if (!wasNamed) {
        (async () => {
          try {
            const { name } = await generateScriptTitle(scriptId);
            scripts.update((list) =>
              (list || []).map((s) => (s.id === scriptId ? { ...s, name } : s))
            );
          } catch (e) {
            console.warn('title generation failed', e);
          }
        })();
      }
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

  /// Play the just-created user turn's widget without cancelling any
  /// autoplay we're about to start for the assistant (playUserWidget does
  /// cancel — that's the right behavior for a manual click but not for the
  /// staged send flow). Runs in the background so send() can move on to
  /// the /reply fetch immediately.
  function playUserTurnFireAndForget(widgetId) {
    (async () => {
      let durationMs = 0;
      try {
        const info = await fetchWidget(widgetId);
        durationMs = info.durationMs || 0;
      } catch {}
      activeClip.set({
        widgetId,
        durationMs: durationMs || 1000,
        startedAt: performance.now(),
      });
      try {
        await playWidget(widgetId);
      } catch (e) {
        if (e?.name !== 'AbortError') console.warn('user turn play failed', e);
      } finally {
        activeClip.update((cur) => (cur?.widgetId === widgetId ? null : cur));
      }
    })();
  }

  /// Clear the whole conversation AND any current draft (typed text +
  /// attached voice memo). Confirms first — this is destructive and the
  /// user has no other undo. The attached widget (if any) is deleted
  /// server-side so it doesn't linger orphaned.
  async function clearAll() {
    if (!confirm('Clear the whole script? This deletes every turn.')) return;
    if (!currentScriptId) return;
    try {
      await resetScript(currentScriptId);
      script.set({ id: currentScriptId, turns: [] });
      latestAssistantId = null;
      cancelAllSequences();
      // Drop any in-progress draft too — the "clear everything" mental
      // model would be surprising if the textarea kept its content or
      // the just-recorded widget stayed attached.
      input = '';
      const wid = attachedWidgetId;
      attachedWidgetId = null;
      if (wid) deleteWidget(wid).catch(() => {});
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

  /// Play the audio attached to a user turn (from the record → transcribe
  /// flow). Same pipewire mic path as SpeechInline — user-initiated so it
  /// preempts any in-flight autoplay AND any Play All queue just like a
  /// manual take click does.
  async function playUserWidget(widgetId) {
    if (!widgetId) return;
    cancelAllSequences();
    try { await stopPlayback(); } catch {}
    // Fetch duration so the user-turn's own progress bar animates over
    // the correct wall-clock span. 1s fallback if the widget lookup fails.
    let durationMs = 0;
    try {
      const info = await fetchWidget(widgetId);
      durationMs = info.durationMs || 0;
    } catch {}
    activeClip.set({
      widgetId,
      durationMs: durationMs || 1000,
      startedAt: performance.now(),
    });
    try {
      await playWidget(widgetId);
    } catch (e) {
      if (e?.name !== 'AbortError') error = `play: ${e.message || e}`;
    } finally {
      activeClip.update((cur) => (cur?.widgetId === widgetId ? null : cur));
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

  /// Svelte action attached to each user-turn wrapper: subscribes to
  /// activeClip and, when THIS wrapper's widgetId becomes active, kicks
  /// off the linear-swipe on its `.user-progress` child and scrolls it
  /// into view. Mirrors what SpeechInline does for its own pills so both
  /// turn types share the visual feel during Play All / manual play.
  function animateOnActive(node, widgetId) {
    // Dedup on startedAt (fresh per play iteration) so Play-All's
    // "replay this same widget" retriggers the sweep — matches the
    // SpeechInline effect's behavior for consistency.
    let lastStartedAt = null;
    const progress = node.querySelector('.user-progress');
    const unsub = activeClip.subscribe((cur) => {
      const nowActive = widgetId && cur && cur.widgetId === widgetId;
      if (!nowActive) {
        if (progress) {
          progress.style.transition = 'none';
          progress.style.transform = 'scaleX(0)';
        }
        lastStartedAt = null;
        return;
      }
      if (cur.startedAt === lastStartedAt) return;
      lastStartedAt = cur.startedAt;
      if (progress) {
        progress.style.transition = 'none';
        progress.style.transform = 'scaleX(0)';
        void progress.offsetWidth;
        progress.style.transition = `transform ${cur.durationMs}ms linear`;
        progress.style.transform = 'scaleX(1)';
      }
      scrollClipIntoView(node);
    });
    return {
      update(newWidgetId) { widgetId = newWidgetId; },
      destroy() { unsub(); },
    };
  }
  // Only enable Play All when there's actually something to play — a
  // user turn with attached voice memo, or any assistant block with at
  // least one rendered take. Otherwise the button would spin uselessly.
  const hasPlayable = $derived(
    turns.some((t) =>
      (t.role === 'user' && t.widget_id) ||
      (t.role === 'assistant' &&
        (t.blocks ?? []).some((b) => (b.takes ?? []).length > 0))
    )
  );
</script>

<!-- Two-pane layout: sidebar (script list) + main (transcript + input).
     The sidebar collapses at narrow widths; a hamburger in the top-left of
     the main pane re-opens it. `sidebarOpen` is derived from viewport
     width by default but overridden by the toggle. -->
<div class="script-shell" class:sidebar-open={sidebarOpen}>
  <ScriptSidebar
    currentScriptId={currentScriptId}
    onchange={onSidebarSelect}
    onCollapse={narrow ? () => sidebarOpen = false : null}
  />
  <div class="script">
    {#if narrow && !sidebarOpen}
      <button
        class="hamburger"
        onclick={() => sidebarOpen = true}
        aria-label="Show script sidebar"
        title="Show script sidebar"
      >
        <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
          <path fill="currentColor" d="M3 6h18v2H3zm0 5h18v2H3zm0 5h18v2H3z"/>
        </svg>
      </button>
    {/if}
    {#if editAgent}
    <!-- System-prompt editor panel. Occupies the top ~half of the
         viewport; the transcript below shrinks to accommodate it. Draft
         binds directly to the local promptDraft and autosaves after a
         short debounce (or on agent switch / component teardown). -->
    <section class="agent-editor">
      <header class="agent-editor-head">
        <div class="agent-editor-title">
          <span>System prompt</span>
          <strong>{activeAgent?.name ?? '—'}</strong>
          {#if activeAgentReadOnly}
            <span class="badge readonly">read-only</span>
          {/if}
        </div>
        <div class="agent-editor-status">
          {#if saving}<span class="hint">saving…</span>{/if}
          {#if !saving && promptDirty}<span class="hint">unsaved</span>{/if}
          {#if !saving && !promptDirty && !activeAgentReadOnly}<span class="hint">saved</span>{/if}
          {#if saveError}<span class="err">{saveError}</span>{/if}
        </div>
      </header>
      <textarea
        class="agent-prompt"
        bind:value={promptDraft}
        oninput={onPromptInput}
        readonly={activeAgentReadOnly}
        placeholder={activeAgentReadOnly
          ? 'The Default agent is read-only. Create a new agent to customize.'
          : 'Write the system prompt this agent will run with…'}
      ></textarea>
      <!-- Voice slots — three character pickers that steer how each role
           reads aloud on the Script page. Each slot resolves to the picked
           character's first (default) voice profile; unset = keep the
           hardcoded DSP preset. Read-only alongside the prompt on the
           Default agent, since Default is cross-project and can't
           reference any specific project's characters. -->
      <div class="voice-slots">
        <label class="slot">
          <span>User voice</span>
          <select
            value={activeAgent?.voice_user ?? ''}
            onchange={(e) => onVoiceSlotChange('voiceUser', e)}
            disabled={activeAgentReadOnly}
            title="Voice for the LLM reading your typed message back (proxy)"
          >
            <option value="">— DSP default (GM proxy) —</option>
            {#each projectCharacters as c (c.id)}
              <option value={c.id}>{c.name}</option>
            {/each}
          </select>
        </label>
        <label class="slot">
          <span>Narrator voice</span>
          <select
            value={activeAgent?.voice_narrator ?? ''}
            onchange={(e) => onVoiceSlotChange('voiceNarrator', e)}
            disabled={activeAgentReadOnly}
            title="Voice for prose outside <speak>…</speak> in the assistant reply"
          >
            <option value="">— DSP default (narrator pitch/pan) —</option>
            {#each projectCharacters as c (c.id)}
              <option value={c.id}>{c.name}</option>
            {/each}
          </select>
        </label>
        <label class="slot">
          <span>Character voice</span>
          <select
            value={activeAgent?.voice_character ?? ''}
            onchange={(e) => onVoiceSlotChange('voiceCharacter', e)}
            disabled={activeAgentReadOnly}
            title="Voice for lines inside <speak>…</speak> in the assistant reply"
          >
            <option value="">— DSP default (base voice) —</option>
            {#each projectCharacters as c (c.id)}
              <option value={c.id}>{c.name}</option>
            {/each}
          </select>
        </label>
        {#if projectCharacters.length === 0 && !activeAgentReadOnly}
          <span class="voice-hint">
            Add characters in the <a href="#/characters">Characters</a> tab to fill these slots.
          </span>
        {/if}
      </div>
    </section>
  {/if}
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
              autoPlayOrd={t.id === autoplayTurnId ? autoplayOrd : null}
              onAutoAdvance={advanceAutoplay}
              onAutoInterrupt={cancelAllSequences}
              agentId={selectedAgentId}
            />
          {:else if t.pending}
            <!-- Animated typing indicator: three staggered dots so it's
                 visually obvious the LLM is working. Swaps to real content
                 as soon as the reply lands (or vanishes with the whole
                 placeholder if the request errors). -->
            <span class="typing" aria-label="thinking…">
              <span class="dot"></span>
              <span class="dot"></span>
              <span class="dot"></span>
            </span>
          {:else}
            <span
              class="user-body"
              class:playing={t.widget_id && $activeClip?.widgetId === t.widget_id}
              use:animateOnActive={t.widget_id}
            >
              <span class="user-progress" aria-hidden="true"></span>
              <span class="plain">{t.content}</span>
              {#if t.widget_id}
                <button
                  class="user-play"
                  onclick={() => playUserWidget(t.widget_id)}
                  title="Play the original recording"
                  aria-label="Play the original recording"
                >
                  <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                    <path fill="currentColor" d="M8 5v14l11-7z" />
                  </svg>
                </button>
              {/if}
            </span>
          {/if}
        </div>
      {/each}
    {/if}
  </div>

  <div class="input-bar">
    {#if isRecording}
      <!-- Recording mode: textarea vanishes, elapsed time + red dot show
           where it used to be. Click Stop (same button that started) to
           finish and drop the transcript into the textarea. -->
      <div class="recording">
        <span class="rec-dot" aria-hidden="true"></span>
        <span class="rec-label">Recording…</span>
        <span class="rec-time">{recordingElapsedLabel}</span>
      </div>
    {:else}
      <textarea
        bind:this={inputEl}
        bind:value={input}
        onkeydown={onKey}
        placeholder={attachedWidgetId ? 'Edit the transcript, then Send…' : 'Speak to the scene… (Enter to send · Shift+Enter for newline)'}
        rows="2"
      ></textarea>
    {/if}
    <div class="input-actions">
      <div class="left-toggles">
        <label class="agent-picker" title="Which stored system prompt to use for the next reply">
          <span>agent</span>
          <select value={selectedAgentId} onchange={onAgentSelectChange}>
            {#each $agents as a (a.id)}
              <option value={a.id}>{a.name}{a.read_only ? ' (built-in)' : ''}</option>
            {/each}
            {#if ($agents ?? []).length > 0}
              <option disabled>──────────</option>
            {/if}
            <option value={CREATE_SENTINEL}>+ Create new agent…</option>
          </select>
        </label>
        <label class="toggle" title="When on, speech blocks in a new reply immediately render their first take. When off, each block waits for you to click ▶.">
          <input type="checkbox" bind:checked={autoRender} />
          <span>auto-render</span>
        </label>
        <label class="toggle" title="Show the current agent's system prompt above the transcript for editing.">
          <input type="checkbox" bind:checked={editAgent} />
          <span>edit agent</span>
        </label>
      </div>
      <div class="right-actions">
      {#if isRecording}
        <button class="record stop" onclick={stopRecord} title="Stop recording">
          <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
            <rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor"/>
          </svg>
          <span>Stop</span>
        </button>
      {:else}
        <!-- Play All walks the whole transcript in order (user memos +
             each assistant block's selected take). Toggles Stop while
             playback is in flight. Disabled when the transcript has no
             playable audio yet. -->
        {#if isPlayingAll}
          <div class="playall-controls">
            <button
              class="playall skip"
              onclick={skipBackward}
              title="Previous clip"
              aria-label="Previous clip"
            >
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                <path fill="currentColor" d="M6 6h2v12H6zm3.5 6l8.5 6V6z"/>
              </svg>
            </button>
            <button
              class="playall playing"
              onclick={togglePlayAll}
              title="Stop playback"
            >
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                <rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/>
              </svg>
              <span class="counter">{playIndex + 1} / {playQueue.length} clips</span>
            </button>
            <button
              class="playall skip"
              onclick={skipForward}
              title="Next clip"
              aria-label="Next clip"
            >
              <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                <path fill="currentColor" d="M6 18l8.5-6L6 6zM16 6h2v12h-2z"/>
              </svg>
            </button>
          </div>
        {:else}
          <button
            class="playall"
            onclick={togglePlayAll}
            disabled={!hasPlayable}
            title="Play the entire conversation"
          >
            <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
              <path fill="currentColor" d="M8 5v14l11-7z" />
            </svg>
            <span>Play All</span>
          </button>
        {/if}
        <!-- Clear always visible in every non-recording state — it wipes
             the whole conversation (plus any in-progress draft). Send vs
             Record swap based on whether there's something to send. -->
        <button class="secondary" onclick={clearAll} title="Clear the whole script">Clear</button>
        {#if isComposed}
          <button onclick={send} disabled={sending}>{sending ? 'Sending…' : 'Send'}</button>
        {:else}
          <button
            class="record"
            onpointerdown={startRecord}
            onclick={startRecord}
            disabled={!hasVoxSource}
            title={hasVoxSource
              ? 'Record a voice memo'
              : 'Route a mic or app stream into the vox sink first (Mixer page).'}
          >
            <svg viewBox="0 0 24 24" width="16" height="16" aria-hidden="true">
              <circle cx="12" cy="12" r="6" fill="currentColor"/>
            </svg>
            <span>Record</span>
          </button>
        {/if}
      {/if}
      </div><!-- .right-actions -->
    </div>
    {#if error}
      <div class="err">{error}</div>
    {/if}
  </div>
  </div><!-- .script -->
</div><!-- .script-shell -->

<style>
  /* Top-level shell: sidebar (fixed width) + main. Full viewport minus
     the sticky menubar so the input stays pinned at the bottom. */
  .script-shell {
    display: grid;
    grid-template-columns: 240px 1fr;
    height: calc(100vh - 52px);
    width: 100%;
  }
  .script {
    position: relative;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    max-width: 900px;
    margin: 0 auto;
    padding: 12px 16px 0;
    gap: 10px;
    /* Cap the main column width but allow it to shrink cleanly on narrow
       viewports. */
    width: 100%;
  }

  /* Hamburger button pinned to the top-left of the main column when the
     sidebar is collapsed. Visible only on narrow screens. */
  .hamburger {
    position: absolute;
    left: 4px;
    top: 4px;
    width: 30px;
    height: 30px;
    padding: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--panel);
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    z-index: 5;
  }
  .hamburger:hover { color: var(--text); border-color: var(--accent); }

  /* At narrow widths the sidebar disappears from the grid (single-column)
     and the main pane fills the viewport. When toggled open, the sidebar
     overlays the main pane with a fixed-position slide-in from the left. */
  @media (max-width: 720px) {
    .script-shell {
      grid-template-columns: 1fr;
    }
    .script-shell > :global(aside.sidebar) {
      display: none;
    }
    .script-shell.sidebar-open > :global(aside.sidebar) {
      display: flex;
      position: fixed;
      left: 0;
      top: 52px;
      bottom: 0;
      width: 240px;
      z-index: 10;
      box-shadow: 4px 0 20px rgba(0, 0, 0, 0.4);
    }
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
    /* Placeholder while waiting on the LLM — muted background, tighter
       padding since it only carries the typing indicator. */
    opacity: 0.85;
  }
  .plain {
    white-space: pre-wrap;
  }

  /* Three-dot typing indicator. Each dot lifts + fades on a shared 1.2s
     cycle, offset by 0.2s so the wave reads left-to-right. */
  .typing {
    display: inline-flex;
    align-items: flex-end;
    gap: 5px;
    height: 1em;
    padding: 0 2px;
  }
  .typing .dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--muted);
    animation: typing-bounce 1.2s ease-in-out infinite;
  }
  .typing .dot:nth-child(2) { animation-delay: 0.2s; }
  .typing .dot:nth-child(3) { animation-delay: 0.4s; }
  @keyframes typing-bounce {
    0%, 60%, 100% {
      transform: translateY(0);
      opacity: 0.35;
    }
    30% {
      transform: translateY(-4px);
      opacity: 1;
    }
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
    /* Do NOT flex-wrap the outer row: the transport buttons on the right
       should stay on one line. Individual toggles inside `.left-toggles`
       still wrap internally when space is tight, so at narrow widths the
       auto-render + edit-agent checkboxes fall UNDER the agent dropdown
       instead of shoving the buttons onto a new row. */
    justify-content: flex-end;
    align-items: center;
    flex-wrap: nowrap;
  }
  /* Group of "settings" controls on the left — picker + toggles. margin-
     right: auto pushes the transport buttons to the right edge. Its own
     flex-wrap: wrap is what lets the checkboxes fall under the picker at
     narrow widths — `min-width: 0` + `flex: 1 1 auto` gives the browser
     permission to shrink this column instead of the buttons. */
  .left-toggles {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-right: auto;
    flex-wrap: wrap;
    flex: 1 1 auto;
    min-width: 0;
  }
  /* Right cluster of buttons — Play All / Clear / Send-or-Record (or the
     lone Stop when recording). Never shrinks, never wraps internally. */
  .right-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: nowrap;
    flex-shrink: 0;
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
    cursor: pointer;
    user-select: none;
  }
  .toggle input { width: auto; margin: 0; cursor: pointer; }
  .agent-picker {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .agent-picker select {
    background: rgba(0, 0, 0, 0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 6px;
    font-size: 12px;
    font-family: inherit;
    text-transform: none;
    letter-spacing: normal;
    cursor: pointer;
  }
  .agent-picker select:focus { outline: none; border-color: var(--accent); }

  /* --- Agent editor panel --------------------------------------------------
     Sits above the transcript when "edit agent" is on. Fixed 50vh so the
     transcript still shows a decent portion below it. Flex column so the
     textarea can flex to fill available height. */
  .agent-editor {
    display: flex;
    flex-direction: column;
    height: 50vh;
    min-height: 0;
    padding: 10px 12px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    gap: 8px;
  }
  .agent-editor-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 10px;
    font-size: 12px;
  }
  .agent-editor-title {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    color: var(--muted);
  }
  .agent-editor-title strong {
    color: var(--text);
    font-weight: 600;
  }
  .badge.readonly {
    padding: 1px 6px;
    border-radius: 999px;
    background: rgba(255, 255, 255, 0.06);
    border: 1px solid var(--border);
    color: var(--muted);
    font-size: 10px;
    letter-spacing: 0.03em;
  }
  .agent-editor-status {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .agent-editor-status .hint {
    color: var(--muted);
    font-size: 11px;
    font-style: italic;
  }
  .agent-editor-status .err {
    color: var(--err);
    font-size: 11px;
  }
  .agent-prompt {
    /* Flex grow inside .agent-editor's 50vh column. `min-height: 0` lets
       the textarea shrink below its intrinsic content height so it fills
       exactly the allotted container rather than pushing it. `max-height:
       none` overrides the generic `textarea { max-height: 30vh }` rule
       below — that cap is for the chat prompt at the bottom, not this
       editor. */
    flex: 1 1 0;
    min-height: 0;
    max-height: none;
    width: 100%;
    resize: none;
    padding: 10px 12px;
    background: rgba(0, 0, 0, 0.28);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 8px;
    font-family: inherit;
    font-size: 13px;
    line-height: 1.5;
    white-space: pre-wrap;
  }
  .agent-prompt:focus { outline: none; border-color: var(--accent); }
  .agent-prompt[readonly] {
    background: rgba(0, 0, 0, 0.15);
    color: var(--muted);
    cursor: not-allowed;
  }

  /* Voice-slot row: three character pickers under the prompt textarea.
     Wraps at narrow widths so slots stack instead of overflowing. */
  .voice-slots {
    display: flex;
    gap: 12px;
    flex-wrap: wrap;
    align-items: flex-end;
  }
  .voice-slots .slot {
    display: inline-flex;
    flex-direction: column;
    gap: 4px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    min-width: 160px;
    flex: 1 1 160px;
  }
  .voice-slots .slot select {
    background: rgba(0, 0, 0, 0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
    text-transform: none;
    letter-spacing: normal;
    cursor: pointer;
  }
  .voice-slots .slot select:focus { outline: none; border-color: var(--accent); }
  .voice-slots .slot select:disabled {
    color: var(--muted);
    background: rgba(0, 0, 0, 0.15);
    cursor: not-allowed;
  }
  .voice-hint {
    display: block;
    width: 100%;
    color: var(--muted);
    font-size: 11px;
    font-style: italic;
  }
  .voice-hint a { color: var(--accent); }

  .err {
    color: var(--err);
    font-size: 12px;
    padding: 2px 4px;
  }

  /* Record + Stop buttons: red-tinted so they read as "start voice capture"
     (and match the SpeakCell recording button convention). */
  button.record {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--err);
    background: rgba(255, 128, 128, 0.10);
    border-color: rgba(255, 128, 128, 0.35);
  }
  button.record:hover:not(:disabled) {
    background: rgba(255, 128, 128, 0.20);
    border-color: var(--err);
  }
  button.record:disabled {
    color: var(--muted);
    background: rgba(255, 255, 255, 0.04);
    border-color: var(--border);
    cursor: not-allowed;
    opacity: 0.65;
  }
  button.record.stop {
    background: rgba(255, 128, 128, 0.22);
    border-color: var(--err);
  }

  /* Recording indicator that replaces the textarea while a session is in
     flight. Blinking red dot + live MM:SS timer. */
  .recording {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px;
    min-height: 44px;
    background: rgba(255, 128, 128, 0.06);
    border: 1px dashed rgba(255, 128, 128, 0.35);
    border-radius: 8px;
    color: var(--text);
    font-size: 14px;
  }
  .rec-dot {
    display: inline-block;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--err);
    animation: rec-blink 1.1s ease-in-out infinite;
  }
  @keyframes rec-blink {
    0%, 100% { opacity: 1; }
    50%      { opacity: 0.35; }
  }
  .rec-label { color: var(--err); font-weight: 500; letter-spacing: 0.02em; }
  .rec-time {
    margin-left: auto;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    font-size: 13px;
  }

  /* Play All button — accent-blue outline, swaps to a filled "playing"
     appearance while a queue is draining so the toggle affordance reads
     clearly. Disabled state matches the shared button:disabled pattern. */
  button.playall {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--accent);
    background: rgba(122, 162, 255, 0.10);
    border-color: rgba(122, 162, 255, 0.35);
  }
  button.playall:hover:not(:disabled) {
    background: rgba(122, 162, 255, 0.22);
    border-color: var(--accent);
  }
  button.playall.playing {
    background: rgba(122, 162, 255, 0.28);
    border-color: var(--accent);
  }
  /* During playback the Play All button becomes a mini transport control:
     [◀◀] [Stop  X / Y clips] [▶▶]. Tight gap so it reads as one unit. */
  .playall-controls {
    display: inline-flex;
    align-items: stretch;
    gap: 4px;
  }
  button.playall.skip {
    padding: 0 8px;
  }
  button.playall .counter {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    margin-left: 4px;
    font-size: 11px;
  }
  button.playall.playing:hover .counter { color: var(--text); }

  /* Small play button appended to a user turn that has an attached voice
     memo. Keeps the transcript readable while making the audio a click
     away. */
  button.user-play {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    margin-left: 8px;
    color: var(--accent);
    background: rgba(122, 162, 255, 0.14);
    border: 1px solid rgba(122, 162, 255, 0.35);
    border-radius: 50%;
    cursor: pointer;
    vertical-align: middle;
    position: relative;
    z-index: 2;
  }
  button.user-play:hover {
    background: rgba(122, 162, 255, 0.28);
    border-color: var(--accent);
  }

  /* Wrapper around a user turn's text + play button. Same progress-overlay
     mechanic as the speech pill — anchored + clipped, difference-blend
     white bar sweeps across during playback. */
  .user-body {
    position: relative;
    display: inline;
    isolation: isolate;
    /* Reserve padding + rounded corners in BOTH states so the transition
       between idle and playing doesn't reflow the surrounding text and
       change line wrap. Only the background swaps on .playing. */
    padding: 1px 4px;
    border-radius: 6px;
    background: transparent;
    transition: background 0.15s ease-in-out;
  }
  .user-body.playing {
    background: rgba(122, 162, 255, 0.06);
  }
  .user-progress {
    /* Same transform-based sweep as .progress in SpeechInline — mix-blend
       flakes on wrapped inline content; layered accent tint reads cleanly
       on both lines and text goes above the overlay. */
    position: absolute;
    inset: 0;
    background: rgba(122, 162, 255, 0.35);
    transform-origin: left center;
    transform: scaleX(0);
    pointer-events: none;
    z-index: 0;
    border-radius: 6px;
  }
  .user-body .plain {
    position: relative;
    z-index: 1;
  }
</style>
