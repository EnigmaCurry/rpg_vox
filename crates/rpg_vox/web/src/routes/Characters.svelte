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
    QWEN3_SPEAKERS,
    QWEN3_LANGUAGES,
  } from '../lib/scenes.svelte.js';

  const project = $derived(currentProject());
  const characters = $derived(currentProjectCharacters());

  let newName = $state('');

  // Two-click delete confirm keyed by character id.
  let deleteArmedFor = $state(null);
  let deleteArmTimer = 0;
  const DELETE_CONFIRM_MS = 2500;

  let uploadErr = $state('');
  // Cap embedded image size — dataURL bloats localStorage fast at multi-MB.
  const MAX_IMAGE_BYTES = 2 * 1024 * 1024;

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
  function onInstructInput(id, ev) {
    updateCharacter(id, { voice: { instruct: ev.currentTarget.value } });
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

  async function readAsDataUrl(file) {
    if (file.size > MAX_IMAGE_BYTES) {
      throw new Error(`image too large (${(file.size / 1024 / 1024).toFixed(1)} MB > 2 MB limit)`);
    }
    return await new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => resolve(r.result);
      r.onerror = () => reject(new Error('failed to read file'));
      r.readAsDataURL(file);
    });
  }

  async function onAvatarPick(id, ev) {
    const file = ev.currentTarget.files?.[0];
    ev.currentTarget.value = '';
    if (!file) return;
    try {
      const dataUrl = await readAsDataUrl(file);
      updateCharacter(id, { avatar: dataUrl });
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
        const dataUrl = await readAsDataUrl(file);
        addCharacterPicture(id, dataUrl, file.name);
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

                <label class="field">
                  <span>Voice-style instruction (optional)</span>
                  <textarea
                    rows="2"
                    placeholder="e.g. calm, whisper, angry, cheerful"
                    value={character.voice.instruct}
                    oninput={(e) => onInstructInput(character.id, e)}
                  ></textarea>
                </label>

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
  .field textarea { resize: vertical; }

  .voice-row {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }

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
