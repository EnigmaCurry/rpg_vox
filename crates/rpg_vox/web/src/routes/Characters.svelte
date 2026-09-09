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
    QWEN3_SPEAKERS,
    QWEN3_LANGUAGES,
    CONFIG_PITCH_RANGE,
    CONFIG_TIME_RANGE,
    CONFIG_DETUNE_RANGE,
    CONFIG_PAN_RANGE,
    CONFIG_GAIN_DB_RANGE,
    CONFIG_DELAY_MS_RANGE,
  } from '../lib/scenes.svelte.js';
  import { uploadImage } from '../lib/api.js';

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
                                  <span>Delay</span>
                                  <input
                                    type="number"
                                    min={CONFIG_DELAY_MS_RANGE.min}
                                    max={CONFIG_DELAY_MS_RANGE.max}
                                    step={CONFIG_DELAY_MS_RANGE.step}
                                    value={config.delayMs}
                                    oninput={(e) => onConfigNumberInput(character.id, profile.id, config.id, 'delayMs', e)}
                                    aria-label="Start delay in milliseconds"
                                    title="Delay this voice's start relative to the profile's mix (ms)"
                                  />
                                  <span class="unit">ms</span>
                                </label>
                              </div>
                            </li>
                          {/each}
                        </ul>
                        {#if configDeleteErr}
                          <div class="err small">{configDeleteErr}</div>
                        {/if}
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

  .config-effects {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
  }
  .fx {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .fx input {
    width: 68px;
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px 6px;
    font-size: 12px;
    font-family: inherit;
    text-transform: none;
    letter-spacing: normal;
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
