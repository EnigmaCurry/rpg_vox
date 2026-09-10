<script>
  import {
    currentProject,
    currentProjectCharacters,
    createCharacter,
    updateCharacter,
    deleteCharacter,
    addCharacterPicture,
    removeCharacterPicture,
    addVoiceProfile,
    renameVoiceProfile,
    deleteVoiceProfile,
    addProfileConfig,
    updateProfileConfig,
    deleteProfileConfig,
    VOICE_MODES,
    QWEN3_SPEAKERS,
    QWEN3_LANGUAGES,
    CONFIG_PITCH_RANGE,
    CONFIG_TIME_RANGE,
    CONFIG_DETUNE_RANGE,
    CONFIG_PAN_RANGE,
    CONFIG_GAIN_DB_RANGE,
    CONFIG_DELAY_MS_RANGE,
    CONFIG_HPF_RANGE,
    CONFIG_LPF_RANGE,
    CONFIG_DRIVE_DB_RANGE,
    CONFIG_CRUSH_BITS_RANGE,
    CONFIG_AM_RATE_RANGE,
    CONFIG_AM_DEPTH_RANGE,
  } from '../lib/scenes.svelte.js';
  import {
    createVoice, deleteVoice, uploadImage, voiceReferenceUrl,
    createSample, deleteSample, sampleAudioUrl,
  } from '../lib/api.js';
  import ProfileTestField from '../components/ProfileTestField.svelte';

  // Human-readable labels for the per-config mode picker. Keys match
  // VOICE_MODES. Copy voices reuse another config's raw synth output so
  // this profile can stack a "core" voice with a pitched double / motor
  // buzz layer without spending extra TTS calls on the copy.
  const MODE_LABELS = {
    presets: 'Presets — 9 named speakers + style instruction',
    clone:   'Clone — 3-second reference audio → voice',
    design:  'Design — free-text voice description',
    copy:    'Copy — reuse another voice’s raw synth output',
    sample:  'Sample — loop an uploaded audio clip (drones, machines, ambience)',
  };

  const project = $derived(currentProject());
  const characters = $derived(currentProjectCharacters());

  let newName = $state('');

  // Two-click delete confirm keyed by character id.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  let uploadErr = $state('');
  // Client-side pre-flight cap. The /images endpoint enforces its own
  // ceiling; keeping a matching cap here means the user sees the error
  // instantly instead of after uploading a doomed-to-fail file.
  const MAX_IMAGE_BYTES = 10 * 1024 * 1024;

  function onCreate(ev) {
    ev.preventDefault();
    if (!project) return;
    const name = newName.trim() || 'New character';
    createCharacter(name);
    newName = '';
  }

  function onNameInput(id, ev) {
    updateCharacter(id, { name: ev.currentTarget.value });
  }

  // ---- Voice profiles + configs -------------------------------------------
  function onProfileNameInput(characterId, profileId, ev) {
    renameVoiceProfile(characterId, profileId, ev.currentTarget.value);
  }
  function onAddProfile(characterId) {
    addVoiceProfile(characterId, 'new profile');
  }
  function onAddConfig(characterId, profileId) {
    addProfileConfig(characterId, profileId);
  }
  function onConfigStringInput(characterId, profileId, configId, field, ev) {
    updateProfileConfig(characterId, profileId, configId, {
      [field]: ev.currentTarget.value,
    });
  }
  function onConfigNumberInput(characterId, profileId, configId, field, ev) {
    updateProfileConfig(characterId, profileId, configId, {
      [field]: ev.currentTarget.valueAsNumber,
    });
  }
  function onConfigBoolInput(characterId, profileId, configId, field, ev) {
    updateProfileConfig(characterId, profileId, configId, {
      [field]: ev.currentTarget.checked,
    });
  }

  // ---- Clone-mode reference audio upload ----------------------------------
  //
  // Two-step: the browser picks a wav (or any audio Gradio can decode),
  // we POST the raw bytes to /voices with the transcription on the query
  // string, and the server does the Qwen3 save_prompt roundtrip. The
  // returned voice_id lands on `config.voiceFileId` — that's what
  // resolve_character_configs later reads on synth. Uploads keyed by
  // `${profileId}:${configId}` show a pending spinner + errors inline
  // so multiple configs' uploads don't clobber each other's status.
  //
  // Max reference size mirrors the server's 8 MB body cap; anything
  // bigger errors client-side without round-tripping.
  const MAX_REFERENCE_BYTES = 8 * 1024 * 1024;
  let voiceUploadStatus = $state({}); // `${p}:${c}` → {loading?, error?}
  let voiceUploadErr = $state('');

  function statusKey(profileId, configId) { return `${profileId}:${configId}`; }
  function setUploadStatus(k, patch) {
    voiceUploadStatus = { ...voiceUploadStatus, [k]: { ...(voiceUploadStatus[k] ?? {}), ...patch } };
  }
  function clearUploadStatus(k) {
    // eslint-disable-next-line no-unused-vars
    const { [k]: _, ...rest } = voiceUploadStatus;
    voiceUploadStatus = rest;
  }

  async function onReferencePick(character, profile, config, ev) {
    const file = ev.currentTarget.files?.[0];
    ev.currentTarget.value = '';
    if (!file) return;
    const k = statusKey(profile.id, config.id);
    if (file.size > MAX_REFERENCE_BYTES) {
      setUploadStatus(k, { error: `too large (${(file.size / 1024 / 1024).toFixed(1)} MB > 8 MB limit)` });
      return;
    }
    setUploadStatus(k, { loading: true, error: null });
    // Grab the previous voice id up front so we can delete it AFTER the
    // new save_prompt succeeds — swapping the id atomically means a
    // failed upload doesn't leave the config voiceless.
    const priorVoiceId = config.voiceFileId;
    // If the user has already typed a transcription, respect it — send
    // it as ref_txt so STT doesn't overwrite their intent. Otherwise
    // omit ref_txt entirely and let the server auto-transcribe; the
    // response includes the STT result which we drop into `description`
    // for review/edit.
    const typedRefTxt = String(config.description ?? '').trim();
    try {
      const resp = await createVoice(file, {
        refTxt: typedRefTxt || null,
        useXvec: true, // TODO: expose a checkbox once we know how it feels in practice
        filename: file.name,
      });
      updateProfileConfig(character.id, profile.id, config.id, {
        voiceFileId: resp.id,
        // Sync `description` back to whatever ref_txt actually fed
        // save_prompt. When we passed a typed value this is a no-op;
        // when STT filled it in, the user now sees the auto-transcript
        // and can edit it in place.
        description: resp.transcript ?? typedRefTxt,
      });
      if (priorVoiceId) {
        deleteVoice(priorVoiceId).catch((e) => console.warn('prior voice cleanup failed', e));
      }
      clearUploadStatus(k);
    } catch (e) {
      setUploadStatus(k, { loading: false, error: e.message || String(e) });
    }
  }

  async function onClearVoice(character, profile, config) {
    if (!config.voiceFileId) return;
    const priorVoiceId = config.voiceFileId;
    updateProfileConfig(character.id, profile.id, config.id, { voiceFileId: null });
    deleteVoice(priorVoiceId).catch((e) => console.warn('voice cleanup failed', e));
  }

  // ---- Sample-mode raw audio upload ---------------------------------------
  //
  // Same UX shape as the clone-mode uploader above, but the server just
  // stores the bytes verbatim under `data/samples/{id}` — no STT roundtrip,
  // no Qwen3 save_prompt. The returned id lands on `config.sampleFileId` so
  // resolve_character_configs picks the clip up on synth. Status is keyed
  // by the same `${profileId}:${configId}` scheme so multi-config uploads
  // don't overwrite each other's spinner / error.
  const MAX_SAMPLE_BYTES = 8 * 1024 * 1024;
  let sampleUploadStatus = $state({}); // `${p}:${c}` → {loading?, error?}

  function setSampleStatus(k, patch) {
    sampleUploadStatus = { ...sampleUploadStatus, [k]: { ...(sampleUploadStatus[k] ?? {}), ...patch } };
  }
  function clearSampleStatus(k) {
    // eslint-disable-next-line no-unused-vars
    const { [k]: _, ...rest } = sampleUploadStatus;
    sampleUploadStatus = rest;
  }

  async function onSamplePick(character, profile, config, ev) {
    const file = ev.currentTarget.files?.[0];
    ev.currentTarget.value = '';
    if (!file) return;
    const k = statusKey(profile.id, config.id);
    if (file.size > MAX_SAMPLE_BYTES) {
      setSampleStatus(k, { error: `too large (${(file.size / 1024 / 1024).toFixed(1)} MB > 8 MB limit)` });
      return;
    }
    setSampleStatus(k, { loading: true, error: null });
    // Swap the sample id atomically so a failed upload doesn't leave the
    // config with a dangling id or a half-deleted server row.
    const priorSampleId = config.sampleFileId;
    try {
      const resp = await createSample(file, { filename: file.name });
      updateProfileConfig(character.id, profile.id, config.id, {
        sampleFileId: resp.id,
      });
      if (priorSampleId) {
        deleteSample(priorSampleId).catch((e) => console.warn('prior sample cleanup failed', e));
      }
      clearSampleStatus(k);
    } catch (e) {
      setSampleStatus(k, { loading: false, error: e.message || String(e) });
    }
  }

  async function onClearSample(character, profile, config) {
    if (!config.sampleFileId) return;
    const priorSampleId = config.sampleFileId;
    updateProfileConfig(character.id, profile.id, config.id, { sampleFileId: null });
    deleteSample(priorSampleId).catch((e) => console.warn('sample cleanup failed', e));
  }

  // Two-tap confirm for profile deletes; keyed as `${characterId}:${profileId}`.
  let profileDeleteArmed = $state(null);
  let profileDeleteTimer = 0;
  let profileDeleteErr = $state('');
  function onDeleteProfile(character, profile) {
    const key = `${character.id}:${profile.id}`;
    if (profileDeleteArmed === key) {
      if (profileDeleteTimer) clearTimeout(profileDeleteTimer);
      profileDeleteArmed = null;
      const res = deleteVoiceProfile(character.id, profile.id);
      if (!res.ok) {
        profileDeleteErr = res.reason;
        setTimeout(() => { profileDeleteErr = ''; }, 2500);
      }
      return;
    }
    profileDeleteArmed = key;
    if (profileDeleteTimer) clearTimeout(profileDeleteTimer);
    profileDeleteTimer = setTimeout(() => {
      profileDeleteArmed = null;
      profileDeleteTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  // Two-tap confirm for config deletes; keyed as `${profileId}:${configId}`.
  let configDeleteArmed = $state(null);
  let configDeleteTimer = 0;
  let configDeleteErr = $state('');
  function onDeleteConfig(character, profile, config) {
    const key = `${profile.id}:${config.id}`;
    if (configDeleteArmed === key) {
      if (configDeleteTimer) clearTimeout(configDeleteTimer);
      configDeleteArmed = null;
      const res = deleteProfileConfig(character.id, profile.id, config.id);
      if (!res.ok) {
        configDeleteErr = res.reason;
        setTimeout(() => { configDeleteErr = ''; }, 2500);
      }
      return;
    }
    configDeleteArmed = key;
    if (configDeleteTimer) clearTimeout(configDeleteTimer);
    configDeleteTimer = setTimeout(() => {
      configDeleteArmed = null;
      configDeleteTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  function armDelete(id) {
    if (deleteArmedFor === id) {
      if (deleteArmTimer) clearTimeout(deleteArmTimer);
      deleteArmedFor = null;
      deleteCharacter(id);
      return;
    }
    deleteArmedFor = id;
    if (deleteArmTimer) clearTimeout(deleteArmTimer);
    deleteArmTimer = setTimeout(() => {
      deleteArmedFor = null;
      deleteArmTimer = 0;
    }, DELETE_CONFIRM_MS);
  }

  // Pre-flight validation + upload. Returns `/images/{id}` on success — the
  // server owns the bytes; the character record only holds the src URL.
  async function uploadFile(file) {
    if (!file.type || !file.type.startsWith('image/')) {
      throw new Error('only image files can be uploaded');
    }
    if (file.size > MAX_IMAGE_BYTES) {
      throw new Error(`image too large (${(file.size / 1024 / 1024).toFixed(1)} MB > 10 MB limit)`);
    }
    const { src } = await uploadImage(file);
    return src;
  }

  async function onAvatarPick(id, ev) {
    const file = ev.currentTarget.files?.[0];
    ev.currentTarget.value = '';
    if (!file) return;
    try {
      const src = await uploadFile(file);
      updateCharacter(id, { avatar: src });
    } catch (e) {
      showUploadErr(e.message);
    }
  }

  function onAvatarClear(id) {
    updateCharacter(id, { avatar: null });
  }

  async function onPicturePick(id, ev) {
    const files = Array.from(ev.currentTarget.files ?? []);
    ev.currentTarget.value = '';
    for (const file of files) {
      try {
        const src = await uploadFile(file);
        addCharacterPicture(id, src, file.name);
      } catch (e) {
        showUploadErr(e.message);
      }
    }
  }

  function showUploadErr(msg) {
    uploadErr = msg;
    setTimeout(() => { uploadErr = ''; }, 4000);
  }
</script>

<section class="wrap">
  <header class="head">
    <h1>Characters</h1>
    <p class="sub">
      {#if project}
        Voices, avatars, and reference images for <b>{project.name}</b>. Scenes require at least one character.
      {:else}
        Load a project from the <a href="#/projects">Projects</a> tab to add characters.
      {/if}
    </p>
  </header>

  {#if project}
    <form class="new" onsubmit={onCreate}>
      <input
        type="text"
        placeholder="New character name"
        bind:value={newName}
        aria-label="New character name"
      />
      <button type="submit">Add character</button>
    </form>

    {#if uploadErr}
      <div class="err">{uploadErr}</div>
    {/if}

    {#if characters.length === 0}
      <div class="empty">No characters yet — add one above.</div>
    {:else}
      <ul class="char-list">
        {#each characters as character (character.id)}
          <li>
            <div class="card">
              <div class="avatar-col">
                <div class="avatar" class:empty={!character.avatar}>
                  {#if character.avatar}
                    <img src={character.avatar} alt="Avatar for {character.name}" />
                  {:else}
                    <span class="ph">no avatar</span>
                  {/if}
                </div>
                <label class="file-btn">
                  {character.avatar ? 'Replace avatar' : 'Upload avatar'}
                  <input
                    type="file"
                    accept="image/*"
                    onchange={(e) => onAvatarPick(character.id, e)}
                  />
                </label>
                {#if character.avatar}
                  <button type="button" class="link-btn" onclick={() => onAvatarClear(character.id)}>
                    Remove avatar
                  </button>
                {/if}
              </div>

              <div class="fields">
                <label class="field">
                  <span>Name</span>
                  <input
                    type="text"
                    value={character.name}
                    oninput={(e) => onNameInput(character.id, e)}
                  />
                </label>

                <div class="profiles">
                  <div class="profiles-head">
                    <span>Voice profiles</span>
                    <button
                      type="button"
                      class="file-btn small"
                      onclick={() => onAddProfile(character.id)}
                      title="Add a new voice profile"
                    >+ add profile</button>
                  </div>

                  <ul class="profile-list">
                    {#each character.voiceProfiles as profile (profile.id)}
                      <li class="profile">
                        <div class="profile-head">
                          <input
                            class="profile-name"
                            type="text"
                            value={profile.name}
                            placeholder="profile name"
                            aria-label="Profile name"
                            oninput={(e) => onProfileNameInput(character.id, profile.id, e)}
                          />
                          <span class="config-count">
                            {profile.configs.length}
                            {profile.configs.length === 1 ? 'voice' : 'voices'}
                          </span>
                          <button
                            type="button"
                            class="mini-btn"
                            onclick={() => onAddConfig(character.id, profile.id)}
                            title="Layer another voice onto this profile (crowd / hive-mind)"
                          >+ voice</button>
                          <button
                            type="button"
                            class="mini-btn danger"
                            class:armed={profileDeleteArmed === `${character.id}:${profile.id}`}
                            class:blocked={character.voiceProfiles.length <= 1}
                            disabled={character.voiceProfiles.length <= 1}
                            onclick={() => onDeleteProfile(character, profile)}
                            title={character.voiceProfiles.length <= 1
                              ? 'At least one profile is required'
                              : (profileDeleteArmed === `${character.id}:${profile.id}` ? 'Click again to confirm' : 'Delete profile')}
                            aria-label="Delete profile"
                          >×</button>
                        </div>

                        <ul class="config-list">
                          {#each profile.configs as config, cfgIdx (config.id)}
                            <li class="config">
                              <div class="config-head">
                                <span class="config-idx">voice {cfgIdx + 1}</span>
                                <label class="mode-picker" title="How this voice renders. Copy borrows another voice's raw synth output — no extra TTS call, but you can still run this voice's own pitch/FX/gain on the copy.">
                                  <span>mode</span>
                                  <select
                                    value={config.mode}
                                    onchange={(e) => onConfigStringInput(character.id, profile.id, config.id, 'mode', e)}
                                  >
                                    {#each VOICE_MODES as m}
                                      <option value={m} title={MODE_LABELS[m]}>{m}</option>
                                    {/each}
                                  </select>
                                </label>
                                <button
                                  type="button"
                                  class="mini-btn danger"
                                  class:armed={configDeleteArmed === `${profile.id}:${config.id}`}
                                  class:blocked={profile.configs.length <= 1}
                                  disabled={profile.configs.length <= 1}
                                  onclick={() => onDeleteConfig(character, profile, config)}
                                  title={profile.configs.length <= 1
                                    ? 'A profile needs at least one voice'
                                    : (configDeleteArmed === `${profile.id}:${config.id}` ? 'Click again to confirm' : 'Delete voice')}
                                  aria-label="Delete voice"
                                >×</button>
                              </div>

                              {#if config.mode === 'presets'}
                                <div class="config-row">
                                  <label class="field">
                                    <span>Speaker</span>
                                    <select
                                      value={config.speaker}
                                      onchange={(e) => onConfigStringInput(character.id, profile.id, config.id, 'speaker', e)}
                                    >
                                      {#each QWEN3_SPEAKERS as spk}
                                        <option value={spk}>{spk}</option>
                                      {/each}
                                    </select>
                                  </label>
                                  <label class="field">
                                    <span>Language</span>
                                    <select
                                      value={config.language}
                                      onchange={(e) => onConfigStringInput(character.id, profile.id, config.id, 'language', e)}
                                    >
                                      {#each QWEN3_LANGUAGES as lang}
                                        <option value={lang}>{lang}</option>
                                      {/each}
                                    </select>
                                  </label>
                                </div>

                                <label class="field">
                                  <span>Instruct</span>
                                  <textarea
                                    class="config-instruct"
                                    rows="2"
                                    placeholder="e.g. calm, whisper, angry, cheerful, robotic"
                                    value={config.instruct}
                                    oninput={(e) => onConfigStringInput(character.id, profile.id, config.id, 'instruct', e)}
                                  ></textarea>
                                </label>
                              {:else if config.mode === 'design'}
                                <label class="field">
                                  <span>Description</span>
                                  <textarea
                                    class="config-instruct"
                                    rows="3"
                                    placeholder="e.g. gravelly old sailor with a slight lisp, warm mid-range, slow and deliberate"
                                    value={config.description}
                                    oninput={(e) => onConfigStringInput(character.id, profile.id, config.id, 'description', e)}
                                  ></textarea>
                                </label>
                                <label class="field">
                                  <span>Language</span>
                                  <select
                                    value={config.language}
                                    onchange={(e) => onConfigStringInput(character.id, profile.id, config.id, 'language', e)}
                                  >
                                    {#each QWEN3_LANGUAGES as lang}
                                      <option value={lang}>{lang}</option>
                                    {/each}
                                  </select>
                                </label>
                              {:else if config.mode === 'clone'}
                                <!-- Reference-audio clone flow. Upload first,
                                     let the server run STT and populate the
                                     transcription — the user then reviews /
                                     edits it in place. If they'd rather type
                                     the transcription first, that still
                                     works: any non-empty description is sent
                                     as `ref_txt` and short-circuits STT.
                                     Description field doubles as the ref_txt
                                     since a clone-mode config only carries
                                     one free-text slot per mode. -->
                                <div class="config-row">
                                  <label class="field">
                                    <span>Reference audio</span>
                                    <div class="voice-file-row">
                                      {#if config.voiceFileId}
                                        <button
                                          type="button"
                                          class="voice-play"
                                          onclick={(ev) => ev.currentTarget.querySelector('audio').play()}
                                          title="Play the uploaded reference"
                                          aria-label="Play the uploaded reference"
                                        >
                                          <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                                            <path fill="currentColor" d="M8 5v14l11-7z"/>
                                          </svg>
                                          <audio src={voiceReferenceUrl(config.voiceFileId)} preload="none"></audio>
                                        </button>
                                        <span class="voice-file-ok" title="Voice file id: {config.voiceFileId}">
                                          ✓ saved
                                        </span>
                                        <button
                                          type="button"
                                          class="link-btn"
                                          onclick={() => onClearVoice(character, profile, config)}
                                        >clear</button>
                                      {:else if voiceUploadStatus[statusKey(profile.id, config.id)]?.loading}
                                        <span class="voice-file-pending">transcribing + rendering…</span>
                                      {:else}
                                        <span class="voice-file-missing">no reference yet</span>
                                      {/if}
                                      <label class="file-btn small">
                                        {config.voiceFileId ? 'replace' : 'upload wav'}
                                        <input
                                          type="file"
                                          accept="audio/*"
                                          onchange={(e) => onReferencePick(character, profile, config, e)}
                                        />
                                      </label>
                                    </div>
                                    {#if voiceUploadStatus[statusKey(profile.id, config.id)]?.error}
                                      <span class="err small">{voiceUploadStatus[statusKey(profile.id, config.id)].error}</span>
                                    {/if}
                                  </label>
                                  <label class="field">
                                    <span>Language</span>
                                    <select
                                      value={config.language}
                                      onchange={(e) => onConfigStringInput(character.id, profile.id, config.id, 'language', e)}
                                    >
                                      {#each QWEN3_LANGUAGES as lang}
                                        <option value={lang}>{lang}</option>
                                      {/each}
                                    </select>
                                  </label>
                                </div>
                                <label class="field">
                                  <span>Reference transcription</span>
                                  <textarea
                                    class="config-instruct"
                                    rows="2"
                                    placeholder="Filled in automatically after upload. Type before upload to skip STT."
                                    value={config.description}
                                    oninput={(e) => onConfigStringInput(character.id, profile.id, config.id, 'description', e)}
                                  ></textarea>
                                </label>
                              {:else if config.mode === 'copy'}
                                <!-- Copy mode: no synth call for this voice. Pick which
                                     sibling voice to reuse; the copy still runs through
                                     its own Stretch + FX + gain chain, so a pitched
                                     double is "copy voice 1 with pitch = -3". Selecting
                                     this voice itself, or a voice that transitively
                                     copies back to this one, will be rejected at
                                     synthesis time. -->
                                <label class="field">
                                  <span>Copy from</span>
                                  <select
                                    value={String(config.copyFromIndex ?? 0)}
                                    onchange={(e) => updateProfileConfig(character.id, profile.id, config.id, { copyFromIndex: Number(e.currentTarget.value) })}
                                    disabled={profile.configs.length <= 1}
                                    title={profile.configs.length <= 1
                                      ? 'Add another voice to this profile before switching a voice to Copy mode'
                                      : 'Which sibling voice to reuse the raw synth output of'}
                                  >
                                    {#each profile.configs as srcCfg, srcIdx (srcCfg.id)}
                                      <option
                                        value={String(srcIdx)}
                                        disabled={srcIdx === cfgIdx}
                                        title={srcIdx === cfgIdx ? 'A voice cannot copy from itself' : ''}
                                      >
                                        voice {srcIdx + 1}{srcIdx === cfgIdx ? ' (self — invalid)' : ''}
                                      </option>
                                    {/each}
                                  </select>
                                </label>
                              {:else if config.mode === 'sample'}
                                <!-- Sample mode: upload a wav / flac / mp3 / ogg. The
                                     server stores the raw bytes and the synth pipeline
                                     decodes + loops the clip to match the longest
                                     synth sibling's duration (or its own natural
                                     length if the profile has no synth voices). The
                                     usual Stretch + FX + gain chain runs on top so
                                     you can HPF/LPF/drive/AM the clip like any voice. -->
                                <label class="field">
                                  <span>Sample audio</span>
                                  <div class="voice-file-row">
                                    {#if config.sampleFileId}
                                      <button
                                        type="button"
                                        class="voice-play"
                                        onclick={(ev) => ev.currentTarget.querySelector('audio').play()}
                                        title="Play the uploaded sample"
                                        aria-label="Play the uploaded sample"
                                      >
                                        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                                          <path fill="currentColor" d="M8 5v14l11-7z"/>
                                        </svg>
                                        <audio src={sampleAudioUrl(config.sampleFileId)} preload="none"></audio>
                                      </button>
                                      <span class="voice-file-ok" title="Sample file id: {config.sampleFileId}">
                                        ✓ saved
                                      </span>
                                      <button
                                        type="button"
                                        class="link-btn"
                                        onclick={() => onClearSample(character, profile, config)}
                                      >clear</button>
                                    {:else if sampleUploadStatus[statusKey(profile.id, config.id)]?.loading}
                                      <span class="voice-file-pending">uploading…</span>
                                    {:else}
                                      <span class="voice-file-missing">no sample yet</span>
                                    {/if}
                                    <label class="file-btn small">
                                      {config.sampleFileId ? 'replace' : 'upload audio'}
                                      <input
                                        type="file"
                                        accept="audio/*"
                                        onchange={(e) => onSamplePick(character, profile, config, e)}
                                      />
                                    </label>
                                  </div>
                                  {#if sampleUploadStatus[statusKey(profile.id, config.id)]?.error}
                                    <span class="err small">{sampleUploadStatus[statusKey(profile.id, config.id)].error}</span>
                                  {/if}
                                </label>
                              {/if}

                              <details class="config-section">
                                <summary>Stretch</summary>
                                <div class="config-effects">
                                  <label class="fx">
                                    <span>Pitch</span>
                                    <input
                                      type="number"
                                      min={CONFIG_PITCH_RANGE.min}
                                      max={CONFIG_PITCH_RANGE.max}
                                      step={CONFIG_PITCH_RANGE.step}
                                      value={config.pitchSemitones}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'pitchSemitones', e)}
                                      aria-label="Pitch shift in semitones"
                                      title="Pitch shift in semitones (0 = no change)"
                                    />
                                    <span class="unit">st</span>
                                  </label>
                                  <label class="fx">
                                    <span>Detune</span>
                                    <input
                                      type="number"
                                      min={CONFIG_DETUNE_RANGE.min}
                                      max={CONFIG_DETUNE_RANGE.max}
                                      step={CONFIG_DETUNE_RANGE.step}
                                      value={config.detuneCents}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'detuneCents', e)}
                                      aria-label="Detune in cents"
                                      title="Fine pitch offset in cents; added to Pitch. 100 cents = 1 semitone."
                                    />
                                    <span class="unit">¢</span>
                                  </label>
                                  <label class="fx">
                                    <span>Speed</span>
                                    <input
                                      type="number"
                                      min={CONFIG_TIME_RANGE.min}
                                      max={CONFIG_TIME_RANGE.max}
                                      step={CONFIG_TIME_RANGE.step}
                                      value={config.timeRatio}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'timeRatio', e)}
                                      aria-label="Time stretch ratio"
                                      title="Duration multiplier (1 = no change, 2 = twice as long, 0.5 = half)"
                                    />
                                    <span class="unit">×</span>
                                  </label>
                                  <label class="fx">
                                    <span>Pan</span>
                                    <input
                                      type="number"
                                      min={CONFIG_PAN_RANGE.min}
                                      max={CONFIG_PAN_RANGE.max}
                                      step={CONFIG_PAN_RANGE.step}
                                      value={config.pan}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'pan', e)}
                                      aria-label="Stereo pan"
                                      title="Stereo pan (-1 = full left, 0 = center, +1 = full right)"
                                    />
                                    <span class="unit">L↔R</span>
                                  </label>
                                  <label class="fx">
                                    <span>Gain</span>
                                    <input
                                      type="number"
                                      min={CONFIG_GAIN_DB_RANGE.min}
                                      max={CONFIG_GAIN_DB_RANGE.max}
                                      step={CONFIG_GAIN_DB_RANGE.step}
                                      value={config.gainDb}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'gainDb', e)}
                                      aria-label="Gain in decibels"
                                      title="Per-voice level (0 = unity)"
                                    />
                                    <span class="unit">dB</span>
                                  </label>
                                  <label class="fx">
                                    <span>Start</span>
                                    <input
                                      type="number"
                                      min={CONFIG_DELAY_MS_RANGE.min}
                                      max={CONFIG_DELAY_MS_RANGE.max}
                                      step={CONFIG_DELAY_MS_RANGE.step}
                                      value={config.delayMs}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'delayMs', e)}
                                      aria-label="Start offset in milliseconds"
                                      title="Nudge this voice's start forward in the profile's mix (ms). Not a delay effect — no repeats, just an offset."
                                    />
                                    <span class="unit">ms</span>
                                  </label>
                                </div>
                              </details>

                              <details class="config-section">
                                <summary>FX</summary>
                                <div class="config-effects">
                                  <label class="fx">
                                    <span>HPF</span>
                                    <input
                                      type="number"
                                      min={CONFIG_HPF_RANGE.min}
                                      max={CONFIG_HPF_RANGE.max}
                                      step={CONFIG_HPF_RANGE.step}
                                      value={config.hpfHz}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'hpfHz', e)}
                                      aria-label="High-pass cutoff in Hz"
                                      title="High-pass cutoff in Hz (0 = disabled). Pair with LPF for a bandpass — try 250 Hz for the radio channel."
                                    />
                                    <span class="unit">Hz</span>
                                  </label>
                                  <label class="fx">
                                    <span>LPF</span>
                                    <input
                                      type="number"
                                      min={CONFIG_LPF_RANGE.min}
                                      max={CONFIG_LPF_RANGE.max}
                                      step={CONFIG_LPF_RANGE.step}
                                      value={config.lpfHz}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'lpfHz', e)}
                                      aria-label="Low-pass cutoff in Hz"
                                      title="Low-pass cutoff in Hz (0 = disabled). Try 3200 Hz for a destroyed-radio timbre, or ~4000 Hz to soften the pitched double."
                                    />
                                    <span class="unit">Hz</span>
                                  </label>
                                  <label class="fx">
                                    <span>Drive</span>
                                    <input
                                      type="number"
                                      min={CONFIG_DRIVE_DB_RANGE.min}
                                      max={CONFIG_DRIVE_DB_RANGE.max}
                                      step={CONFIG_DRIVE_DB_RANGE.step}
                                      value={config.driveDb}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'driveDb', e)}
                                      aria-label="Saturation drive in decibels"
                                      title="tanh saturation drive in dB (0 = disabled). Adds harmonics — try 6–12 dB for tube grit."
                                    />
                                    <span class="unit">dB</span>
                                  </label>
                                  <label class="fx">
                                    <span>Bits</span>
                                    <input
                                      type="number"
                                      min={CONFIG_CRUSH_BITS_RANGE.min}
                                      max={CONFIG_CRUSH_BITS_RANGE.max}
                                      step={CONFIG_CRUSH_BITS_RANGE.step}
                                      value={config.crushBits}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'crushBits', e)}
                                      aria-label="Bit-crush quantization depth"
                                      title="Effective bit depth (0 = disabled, 16 = essentially clean, 6–10 = crunchy vox-caster)."
                                    />
                                    <span class="unit">bit</span>
                                  </label>
                                  <label class="fx">
                                    <span>AM Rate</span>
                                    <input
                                      type="number"
                                      min={CONFIG_AM_RATE_RANGE.min}
                                      max={CONFIG_AM_RATE_RANGE.max}
                                      step={CONFIG_AM_RATE_RANGE.step}
                                      value={config.amRateHz}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'amRateHz', e)}
                                      aria-label="Amplitude modulation rate in Hz"
                                      title="Tremolo / motor-buzz rate in Hz (0 = disabled). ~47 Hz is the classic Adeptus-Mechanicum servo buzz; avoid 50/60 Hz."
                                    />
                                    <span class="unit">Hz</span>
                                  </label>
                                  <label class="fx">
                                    <span>AM Depth</span>
                                    <input
                                      type="number"
                                      min={CONFIG_AM_DEPTH_RANGE.min}
                                      max={CONFIG_AM_DEPTH_RANGE.max}
                                      step={CONFIG_AM_DEPTH_RANGE.step}
                                      value={config.amDepth}
                                      oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'amDepth', e)}
                                      aria-label="Amplitude modulation depth"
                                      title="Depth of the AM modulation (0 = disabled, 1 = full ring-mod). 0.2–0.4 gives a subtle motor throb."
                                    />
                                    <span class="unit">×</span>
                                  </label>
                                </div>
                              </details>
                            </li>
                          {/each}
                        </ul>
                        {#if configDeleteErr}
                          <div class="err small">{configDeleteErr}</div>
                        {/if}
                        <ProfileTestField
                          characterId={character.id}
                          profileId={profile.id}
                        />
                      </li>
                    {/each}
                  </ul>
                  {#if profileDeleteErr}
                    <div class="err small">{profileDeleteErr}</div>
                  {/if}
                </div>

                <div class="pictures">
                  <div class="pictures-head">
                    <span>Reference pictures</span>
                    <label class="file-btn small">
                      + add
                      <input
                        type="file"
                        accept="image/*"
                        multiple
                        onchange={(e) => onPicturePick(character.id, e)}
                      />
                    </label>
                  </div>
                  {#if character.pictures.length === 0}
                    <div class="empty small">no attachments</div>
                  {:else}
                    <ul class="thumbs">
                      {#each character.pictures as pic (pic.id)}
                        <li class="thumb">
                          <img src={pic.dataUrl} alt={pic.name || 'reference'} title={pic.name} />
                          <button
                            type="button"
                            class="thumb-del"
                            onclick={() => removeCharacterPicture(character.id, pic.id)}
                            aria-label="Remove picture"
                            title="Remove picture"
                          >×</button>
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              </div>

              <div class="actions">
                <button
                  type="button"
                  class="del"
                  class:armed={deleteArmedFor === character.id}
                  onclick={() => armDelete(character.id)}
                  title={deleteArmedFor === character.id ? 'Click again to delete' : 'Delete character'}
                >{deleteArmedFor === character.id ? 'really?' : 'delete'}</button>
              </div>
            </div>
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
  }
  .sub a { color: var(--accent); }

  .new { display: flex; gap: 8px; }
  .new input {
    flex: 1;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 8px 10px;
    font-size: 13px;
    font-family: inherit;
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
  .new button:hover { background: rgba(122,162,255,0.22); }

  .empty {
    padding: 24px;
    color: var(--muted);
    font-size: 13px;
    text-align: center;
    border: 1px dashed var(--border);
    border-radius: 8px;
  }
  .empty.small { padding: 10px; font-size: 12px; }
  .err {
    padding: 8px 12px;
    color: var(--err);
    font-size: 12px;
    border: 1px solid var(--err);
    border-radius: 6px;
    background: rgba(255,128,128,0.06);
  }
  .err.small { padding: 4px 8px; font-size: 11px; }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .char-list {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }

  .card {
    display: grid;
    grid-template-columns: 130px 1fr auto;
    gap: 16px;
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--panel);
  }

  .avatar-col {
    display: flex;
    flex-direction: column;
    gap: 6px;
    align-items: stretch;
  }
  .avatar {
    width: 130px;
    height: 130px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: rgba(0,0,0,0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
  }
  .avatar img { width: 100%; height: 100%; object-fit: cover; }
  .avatar .ph { color: var(--muted); font-size: 11px; }

  .fields {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
  }
  .field { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .field > span {
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .field input,
  .field select,
  .field textarea {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
  }
  .field input:focus,
  .field select:focus,
  .field textarea:focus { outline: none; border-color: var(--accent); }

  .profiles { display: flex; flex-direction: column; gap: 8px; }
  .profiles-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .profile-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .profile {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: rgba(0,0,0,0.15);
  }
  .profile-head {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .profile-name {
    flex: 1;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
    font-weight: 600;
  }
  .profile-name:focus { outline: none; border-color: var(--accent); }

  /* Mode picker in the profile head — small inline select, same visual
     weight as `.mini-btn` so it doesn't dominate the row. */
  .mode-picker {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .mode-picker select {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 3px 6px;
    font-size: 11px;
    font-family: inherit;
    text-transform: none;
    letter-spacing: normal;
    cursor: pointer;
  }
  .mode-picker select:focus { outline: none; border-color: var(--accent); }

  /* Clone-mode reference audio status + upload trigger. */
  .voice-file-row {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 4px 0;
    text-transform: none;
    letter-spacing: normal;
    font-size: 12px;
  }
  .voice-file-ok      { color: var(--accent); font-weight: 500; }
  .voice-file-pending { color: var(--muted); font-style: italic; }
  .voice-file-missing { color: var(--muted); }
  /* Play button for review-listening to the uploaded reference wav.
     Same footprint as the tiny mini-btn action buttons elsewhere, but
     accent-tinted so users can tell it's an action, not decoration.
     The embedded `<audio>` element is invisible — the button drives
     playback via `querySelector('audio').play()`. */
  .voice-play {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    color: var(--accent);
    background: rgba(122, 162, 255, 0.14);
    border: 1px solid rgba(122, 162, 255, 0.35);
    border-radius: 50%;
    cursor: pointer;
    position: relative;
  }
  .voice-play:hover { background: rgba(122, 162, 255, 0.28); border-color: var(--accent); }
  .voice-play audio { display: none; }

  .config-count {
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .mini-btn {
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 3px 8px;
    font-size: 11px;
    font-family: inherit;
    cursor: pointer;
  }
  .mini-btn:hover:not(:disabled) { color: var(--accent); border-color: var(--accent); }
  .mini-btn.danger:hover:not(:disabled),
  .mini-btn.danger.armed {
    color: var(--err);
    border-color: var(--err);
    background: rgba(255,128,128,0.10);
  }
  .mini-btn.blocked,
  .mini-btn:disabled { opacity: 0.4; cursor: not-allowed; }

  .config-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .config {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    border: 1px dashed var(--border);
    border-radius: 6px;
    background: rgba(0,0,0,0.15);
  }
  .config-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .config-idx {
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .config-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .config-instruct {
    resize: vertical;
    width: 100%;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
  }
  .config-instruct:focus { outline: none; border-color: var(--accent); }

  /* Collapsible "Stretch" / "FX" groups per config. Native <details> so the
     browser handles state; the summary chip mimics the .fx label styling so
     it visually blends with the row above and below. */
  .config-section {
    border-top: 1px dashed var(--border);
    padding-top: 6px;
  }
  .config-section > summary {
    cursor: pointer;
    list-style: none;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 2px 0;
    user-select: none;
  }
  .config-section > summary::-webkit-details-marker { display: none; }
  .config-section > summary::before {
    content: '▸';
    font-size: 10px;
    line-height: 1;
    color: var(--muted);
    transition: transform 0.15s ease;
  }
  .config-section[open] > summary::before { transform: rotate(90deg); }
  .config-section > summary:hover { color: var(--accent); }
  .config-section[open] > summary { color: var(--accent); }
  .config-section > .config-effects { margin-top: 6px; }

  /* Fixed 2-column grid so meaningful pairs (HPF/LPF, AM Rate/AM Depth)
     always land on the same row regardless of viewport width. Each .fx
     cell is itself a 3-column mini-grid (label | number input | unit)
     so labels, inputs, and units also align across rows even when knobs
     have different name/unit widths. With Doubler gone the FX grid is a
     clean 3×2 (HPF/LPF, Drive/Bits, AM Rate/AM Depth) so no spacer cell
     is needed anymore. */
  .config-effects {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 6px 14px;
  }
  .fx {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 68px 2.5em;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .fx > span:first-child {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fx input {
    width: 100%;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 6px;
    font-size: 12px;
    font-family: inherit;
    text-transform: none;
    letter-spacing: normal;
    box-sizing: border-box;
  }
  .fx input:focus { outline: none; border-color: var(--accent); }
  .fx .unit { text-transform: none; letter-spacing: normal; color: var(--muted); }

  .pictures { display: flex; flex-direction: column; gap: 6px; }
  .pictures-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .thumbs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .thumb {
    position: relative;
    width: 64px;
    height: 64px;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
    background: rgba(0,0,0,0.35);
  }
  .thumb img { width: 100%; height: 100%; object-fit: cover; }
  .thumb-del {
    position: absolute;
    top: 2px;
    right: 2px;
    width: 18px;
    height: 18px;
    padding: 0;
    border-radius: 50%;
    border: none;
    background: rgba(0,0,0,0.65);
    color: #fff;
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .thumb-del:hover { background: var(--err); }

  .file-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 6px 10px;
    background: transparent;
    color: var(--muted);
    border: 1px dashed var(--border);
    border-radius: 6px;
    cursor: pointer;
    font-size: 12px;
  }
  .file-btn:hover { color: var(--accent); border-color: var(--accent); }
  .file-btn.small { padding: 3px 8px; font-size: 11px; }
  .file-btn input { display: none; }

  .link-btn {
    background: transparent;
    color: var(--muted);
    border: none;
    padding: 0;
    font-size: 11px;
    cursor: pointer;
    text-decoration: underline;
  }
  .link-btn:hover { color: var(--err); }

  .actions {
    display: flex;
    flex-direction: column;
    justify-content: flex-start;
  }
  .del {
    padding: 4px 10px;
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 6px;
    font-size: 12px;
    cursor: pointer;
    font-family: inherit;
  }
  .del:hover { color: var(--err); border-color: var(--err); }
  .del.armed {
    color: var(--err);
    border-color: var(--err);
    background: rgba(255,128,128,0.10);
  }
</style>
