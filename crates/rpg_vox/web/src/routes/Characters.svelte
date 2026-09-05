<script>
  import {
    scenesState,
    currentProject,
    currentProjectCharacters,
    createCharacter,
    updateCharacter,
    deleteCharacter,
    addCharacterPicture,
    removeCharacterPicture,
    addCharacterStyle,
    renameCharacterStyle,
    setCharacterStyleInstruct,
    setCharacterStyleEffect,
    deleteCharacterStyle,
    QWEN3_SPEAKERS,
    QWEN3_LANGUAGES,
    STYLE_PITCH_RANGE,
    STYLE_TIME_RANGE,
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
  function onSpeakerChange(id, ev) {
    updateCharacter(id, { voice: { speaker: ev.currentTarget.value } });
  }
  function onLanguageChange(id, ev) {
    updateCharacter(id, { voice: { language: ev.currentTarget.value } });
  }
  function onStyleNameInput(characterId, styleId, ev) {
    renameCharacterStyle(characterId, styleId, ev.currentTarget.value);
  }
  function onStyleInstructInput(characterId, styleId, ev) {
    setCharacterStyleInstruct(characterId, styleId, ev.currentTarget.value);
  }
  function onStylePitchInput(characterId, styleId, ev) {
    setCharacterStyleEffect(characterId, styleId, {
      pitchSemitones: ev.currentTarget.valueAsNumber,
    });
  }
  function onStyleTimeInput(characterId, styleId, ev) {
    setCharacterStyleEffect(characterId, styleId, {
      timeRatio: ev.currentTarget.valueAsNumber,
    });
  }
  function onAddStyle(characterId) {
    addCharacterStyle(characterId, 'new style');
  }

  // Two-tap confirm for style deletes; keyed as `${characterId}:${styleId}`
  // so multiple armed rows never collide.
  let styleDeleteArmed = $state(null);
  let styleDeleteTimer = 0;
  let styleDeleteErr = $state('');
  function onDeleteStyle(characterId, styleId) {
    const key = `${characterId}:${styleId}`;
    if (styleDeleteArmed === key) {
      if (styleDeleteTimer) clearTimeout(styleDeleteTimer);
      styleDeleteArmed = null;
      const res = deleteCharacterStyle(characterId, styleId);
      if (!res.ok) {
        styleDeleteErr = res.reason;
        setTimeout(() => { styleDeleteErr = ''; }, 2500);
      }
      return;
    }
    styleDeleteArmed = key;
    if (styleDeleteTimer) clearTimeout(styleDeleteTimer);
    styleDeleteTimer = setTimeout(() => {
      styleDeleteArmed = null;
      styleDeleteTimer = 0;
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
      <ul>
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

                <div class="voice-row">
                  <label class="field">
                    <span>Voice speaker</span>
                    <select
                      value={character.voice.speaker}
                      onchange={(e) => onSpeakerChange(character.id, e)}
                    >
                      {#each QWEN3_SPEAKERS as spk}
                        <option value={spk}>{spk}</option>
                      {/each}
                    </select>
                  </label>

                  <label class="field">
                    <span>Language</span>
                    <select
                      value={character.voice.language}
                      onchange={(e) => onLanguageChange(character.id, e)}
                    >
                      {#each QWEN3_LANGUAGES as lang}
                        <option value={lang}>{lang}</option>
                      {/each}
                    </select>
                  </label>
                </div>

                <div class="styles">
                  <div class="styles-head">
                    <span>Voice-style instructions</span>
                    <button
                      type="button"
                      class="file-btn small"
                      onclick={() => onAddStyle(character.id)}
                      title="Add a new voice-style slot"
                    >+ add style</button>
                  </div>
                  <ul class="style-list">
                    {#each character.voice.styles as style (style.id)}
                      <li class="style-row">
                        <input
                          class="style-name"
                          type="text"
                          value={style.name}
                          placeholder="style name"
                          aria-label="Style name"
                          oninput={(e) => onStyleNameInput(character.id, style.id, e)}
                        />
                        <div class="style-body">
                          <textarea
                            class="style-instruct"
                            rows="2"
                            placeholder="e.g. calm, whisper, angry, cheerful"
                            value={style.instruct}
                            oninput={(e) => onStyleInstructInput(character.id, style.id, e)}
                          ></textarea>
                          <!-- Post-processing effects. Apply to any backend
                               (Piper included) since they run on the mono PCM
                               after synthesis. Identity values (0 / 1) skip
                               the DSP call on the server. -->
                          <div class="style-effects">
                            <label class="fx">
                              <span>Pitch</span>
                              <input
                                type="number"
                                min={STYLE_PITCH_RANGE.min}
                                max={STYLE_PITCH_RANGE.max}
                                step={STYLE_PITCH_RANGE.step}
                                value={style.pitchSemitones}
                                oninput={(e) => onStylePitchInput(character.id, style.id, e)}
                                aria-label="Pitch shift in semitones"
                                title="Pitch shift in semitones (0 = no change)"
                              />
                              <span class="unit">st</span>
                            </label>
                            <label class="fx">
                              <span>Speed</span>
                              <input
                                type="number"
                                min={STYLE_TIME_RANGE.min}
                                max={STYLE_TIME_RANGE.max}
                                step={STYLE_TIME_RANGE.step}
                                value={style.timeRatio}
                                oninput={(e) => onStyleTimeInput(character.id, style.id, e)}
                                aria-label="Time stretch ratio"
                                title="Duration multiplier (1 = no change, 2 = twice as long, 0.5 = half)"
                              />
                              <span class="unit">×</span>
                            </label>
                          </div>
                        </div>
                        <button
                          type="button"
                          class="style-del"
                          class:armed={styleDeleteArmed === `${character.id}:${style.id}`}
                          class:blocked={character.voice.styles.length <= 1}
                          disabled={character.voice.styles.length <= 1}
                          onclick={() => onDeleteStyle(character.id, style.id)}
                          title={character.voice.styles.length <= 1
                            ? 'At least one style is required'
                            : (styleDeleteArmed === `${character.id}:${style.id}` ? 'Click again to confirm' : 'Delete style')}
                          aria-label="Delete style"
                        >×</button>
                      </li>
                    {/each}
                  </ul>
                  {#if styleDeleteErr}
                    <div class="err small">{styleDeleteErr}</div>
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

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
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
  .field select {
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
  .field select:focus { outline: none; border-color: var(--accent); }

  .voice-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }

  .styles { display: flex; flex-direction: column; gap: 6px; }
  .styles-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: 11px;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .style-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .style-row {
    display: grid;
    grid-template-columns: 130px 1fr auto;
    gap: 6px;
    align-items: start;
  }
  .style-name,
  .style-instruct {
    background: rgba(0,0,0,0.35);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    font-size: 13px;
    font-family: inherit;
    min-width: 0;
  }
  .style-name:focus,
  .style-instruct:focus { outline: none; border-color: var(--accent); }
  .style-instruct { resize: vertical; width: 100%; }

  .style-body {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .style-effects {
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
    width: 70px;
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
  .style-del {
    align-self: stretch;
    background: transparent;
    color: var(--muted);
    border: 1px solid var(--border);
    border-radius: 6px;
    width: 28px;
    padding: 0;
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
    font-family: inherit;
  }
  .style-del:hover:not(:disabled) { color: var(--err); border-color: var(--err); }
  .style-del.armed {
    color: var(--err);
    border-color: var(--err);
    background: rgba(255,128,128,0.10);
  }
  .style-del.blocked,
  .style-del:disabled { opacity: 0.4; cursor: not-allowed; }

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
