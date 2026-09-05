// Client-side store for Project → Scene → Lane / Clip organization,
// plus per-project Characters.
//
// Data shape (v3):
//   project   = { id, name }
//   character = {
//     id, projectId, name,
//     voice: {
//       speaker, language,
//       styles: [{ id, name, instruct }], // >=1; first is the fallback "default"
//     },
//     avatar: dataUrl | null,
//     pictures: [{ id, dataUrl, name }],
//   }
//   scene     = {
//     id, name, projectId,
//     lanes: [{ id, voice }],            // columns; no clips owned here
//     clips: [{ id, laneId, widgetId, text, styleId }],   // ordered timeline
//   }
//
// Older sessions stored a single `voice.instruct` string on the character and
// a `clip.instruct` override string on the clip. On load we promote the
// character's instruct to a "default" style and leave the legacy clip.instruct
// in place as a resolver fallback so pre-existing clips still render as they
// did before styles existed.
//
// Projects group scenes; the selected project drives what Scenes.svelte and
// its sidebar show. Lanes are just column labels + a voice identity. Every
// clip belongs to a scene-level ordered list; its `laneId` picks which
// column it renders in. Vertical position in the UI = index in `scene.clips`
// = time. Two clips never share a "row" — voices take turns.
//
// v1 (older sessions) stored clips under lane.clips[]; the loader flattens
// those into the scene-level list in first-seen order. v2 stored scenes at
// the top level without a project. On first load into v3 we synthesize a
// "Default" project and adopt any orphan scenes into it so nothing is lost.
//
// Individual clip audio + text/instruct still live server-side under
// /widgets/{id}; deleting a scene or project cascades DELETE /widgets/{id}
// for every rendered clip so backend rows and WAV files don't leak. The
// backend delete is idempotent, so unknown/already-gone ids no-op safely.
// Character avatars and reference pictures likewise live server-side under
// /images/{id}; the character record just stores the src URL.
//
// Uses Svelte 5 `$state` (hence the `.svelte.js` extension) so in-place
// mutations propagate through the reactivity graph without callers needing
// to do immutable spreads.
//
// Persistence split:
//   - projects / scenes / characters   → server (/state, sqlite blob)
//   - selectedProjectId / selectedSceneId → localStorage (per-browser prefs)

import { deleteWidget, deleteImage, getAppState, putAppState } from './api.js';

// Per-browser UI selection only. All shared data lives server-side.
const SELECTION_KEY = 'rpg-vox:selection:v1';
// PUT /state debounce — high enough that keystroke storms coalesce, low
// enough that a browser close/refresh soon after a mutation still catches it.
const STATE_SAVE_DEBOUNCE_MS = 400;

// Fire DELETE /widgets/{id} for every rendered clip in `scene`, concurrently.
// Idempotent server-side, so a stale id just no-ops. Returns { failed }
// (count of network/server failures); client-side removal happens regardless
// so the user isn't stranded with a scene they asked to delete.
async function cascadeDeleteSceneWidgets(scene) {
  const ids = Array.from(new Set(
    (scene.clips || []).map((c) => c.widgetId).filter(Boolean),
  ));
  if (ids.length === 0) return { failed: 0 };
  const results = await Promise.allSettled(ids.map((id) => deleteWidget(id)));
  const failed = results.filter((r) => r.status === 'rejected').length;
  if (failed > 0) {
    // Log the first failure so the console still has actionable detail —
    // callers get an aggregate count for user-facing messaging.
    const first = results.find((r) => r.status === 'rejected');
    console.warn('widget cascade delete: some clips failed', first?.reason);
  }
  return { failed };
}

// Extract the image id from a stored src URL. Server-uploaded images live at
// `/images/{uuid}`; legacy dataURL avatars (`data:image/...`) have no id and
// don't need a server-side delete. Returns null when the src isn't a
// server-hosted image (dataURL, absolute URL, empty).
function imageIdFromSrc(src) {
  if (typeof src !== 'string') return null;
  const m = /^\/images\/([^\/?#]+)$/.exec(src);
  return m ? m[1] : null;
}

// Fire DELETE /images/{id} for every server-hosted image referenced by a
// character (avatar + reference pictures). Same fire-and-forget contract as
// the widget cascade above: local state is removed regardless of server
// success, so the user isn't blocked by a flaky network.
async function cascadeDeleteCharacterImages(character) {
  const ids = new Set();
  const avatarId = imageIdFromSrc(character?.avatar);
  if (avatarId) ids.add(avatarId);
  for (const pic of character?.pictures ?? []) {
    const pid = imageIdFromSrc(pic?.dataUrl);
    if (pid) ids.add(pid);
  }
  if (ids.size === 0) return { failed: 0 };
  const results = await Promise.allSettled(
    Array.from(ids, (id) => deleteImage(id)),
  );
  const failed = results.filter((r) => r.status === 'rejected').length;
  if (failed > 0) {
    const first = results.find((r) => r.status === 'rejected');
    console.warn('image cascade delete: some images failed', first?.reason);
  }
  return { failed };
}

function uuid() {
  if (typeof crypto !== 'undefined' && crypto.randomUUID) return crypto.randomUUID();
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    const v = c === 'x' ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

function emptyState() {
  return {
    projects: [],
    selectedProjectId: null,
    scenes: [],
    selectedSceneId: null,
    characters: [],
  };
}

// Qwen3-TTS enum vocab, mirrored from crates/rpg_vox/src/tts/qwen3.rs.
export const QWEN3_SPEAKERS = [
  'Serena', 'Vivian', 'Uncle Fu', 'Ryan', 'Aiden',
  'Ono Anna', 'Sohee', 'Eric', 'Dylan',
];
export const QWEN3_LANGUAGES = [
  'Auto', 'Chinese', 'English', 'German', 'Italian', 'Portuguese',
  'Spanish', 'Japanese', 'Korean', 'French', 'Russian',
];

// Effect defaults live here so every place that constructs or sanitizes a
// style stays in sync. `pitchSemitones = 0` and `timeRatio = 1` are the
// identity values the server short-circuits on.
export const STYLE_PITCH_RANGE = { min: -24, max: 24, step: 1 };
export const STYLE_TIME_RANGE  = { min: 0.25, max: 4, step: 0.05 };

function makeStyle(name = 'default', instruct = '') {
  return { id: uuid(), name, instruct, pitchSemitones: 0, timeRatio: 1 };
}

function clampNumber(value, fallback, min, max) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

function defaultVoice() {
  return {
    speaker: QWEN3_SPEAKERS[0],
    language: QWEN3_LANGUAGES[0],
    styles: [makeStyle('default')],
  };
}

function migrateScene(scene) {
  scene.lanes = Array.isArray(scene.lanes) ? scene.lanes : [];
  // v1 held clips under each lane; hoist them to scene.clips and drop the
  // per-lane arrays. Concatenation order is stable — lane order preserved.
  if (!Array.isArray(scene.clips)) {
    scene.clips = [];
    for (const lane of scene.lanes) {
      for (const c of (lane.clips || [])) {
        scene.clips.push({
          id: c.id ?? uuid(),
          laneId: lane.id,
          widgetId: c.widgetId ?? null,
          text: c.text ?? '',
          styleId: c.styleId ?? null,
          // Preserved so the resolver can honor pre-styles overrides.
          instruct: c.instruct ?? '',
        });
      }
      delete lane.clips;
    }
  } else {
    // Already v2+ — sanity-scrub in case an older bug left orphaned laneIds.
    const validLaneIds = new Set(scene.lanes.map((l) => l.id));
    scene.clips = scene.clips
      .filter((c) => validLaneIds.has(c.laneId))
      .map((c) => ({ styleId: null, ...c }));
  }
  if (typeof scene.pauseMs !== 'number') scene.pauseMs = DEFAULT_PAUSE_MS;
}

export const DEFAULT_PAUSE_MS = 300;
export const PAUSE_OPTIONS = [0, 150, 300, 500, 1000, 2000];

// Apply the same shape sanitization the old localStorage load used to,
// against a payload from any source (server /state, legacy localStorage
// blob, or empty). Returns a fully-populated `{ projects, scenes, characters }`
// with orphan scenes/characters dropped and character sub-fields backfilled.
// Selection (`selectedProjectId`/`selectedSceneId`) is handled separately.
function normalizePayload(parsed) {
  if (!parsed || typeof parsed !== 'object') {
    return { projects: [], scenes: [], characters: [] };
  }
  const rawScenes = Array.isArray(parsed.scenes) ? parsed.scenes : [];
  for (const scene of rawScenes) migrateScene(scene);

  let projects = Array.isArray(parsed.projects) ? parsed.projects : [];

  // v2 → v3: any scene missing projectId gets adopted by a Default project.
  const orphans = rawScenes.filter((s) => !s.projectId);
  if (orphans.length > 0) {
    let defaultProject = projects.find((p) => p.name === 'Default');
    if (!defaultProject) {
      defaultProject = { id: uuid(), name: 'Default' };
      projects.push(defaultProject);
    }
    for (const s of orphans) s.projectId = defaultProject.id;
  }

  const validProjectIds = new Set(projects.map((p) => p.id));
  const scenes = rawScenes.filter((s) => validProjectIds.has(s.projectId));

  const rawChars = Array.isArray(parsed.characters) ? parsed.characters : [];
  const characters = rawChars
    .filter((c) => c && validProjectIds.has(c.projectId))
    .map((c) => {
      // v3: single voice.instruct string → styles = [{id, name:'default', instruct}].
      // Existing style arrays are kept as-is (sanitized). Empty/missing → seeded
      // with an empty "default" so the resolver always has something to return.
      let styles = Array.isArray(c.voice?.styles)
        ? c.voice.styles
            .filter((s) => s && typeof s === 'object')
            .map((s) => ({
              id: typeof s.id === 'string' ? s.id : uuid(),
              name: typeof s.name === 'string' && s.name ? s.name : 'unnamed',
              instruct: typeof s.instruct === 'string' ? s.instruct : '',
              // Pre-effects styles are missing these — fall back to identity
              // values so old projects load with no audio changes.
              pitchSemitones: clampNumber(
                s.pitchSemitones,
                0,
                STYLE_PITCH_RANGE.min,
                STYLE_PITCH_RANGE.max,
              ),
              timeRatio: clampNumber(
                s.timeRatio,
                1,
                STYLE_TIME_RANGE.min,
                STYLE_TIME_RANGE.max,
              ),
            }))
        : null;
      if (!styles || styles.length === 0) {
        styles = [makeStyle('default', c.voice?.instruct ?? '')];
      }
      return {
        id: c.id ?? uuid(),
        projectId: c.projectId,
        name: c.name ?? 'Unnamed',
        voice: {
          speaker: c.voice?.speaker ?? QWEN3_SPEAKERS[0],
          language: c.voice?.language ?? QWEN3_LANGUAGES[0],
          styles,
        },
        // `avatar` and `pictures[].dataUrl` are img-src strings. New uploads
        // land as `/images/{id}`; pre-migration values are `data:image/...`.
        avatar: typeof c.avatar === 'string' ? c.avatar : null,
        pictures: Array.isArray(c.pictures)
          ? c.pictures.filter((p) => p && typeof p.dataUrl === 'string').map((p) => ({
              id: p.id ?? uuid(),
              dataUrl: p.dataUrl,
              name: p.name ?? '',
            }))
          : [],
      };
    });

  return { projects, scenes, characters };
}

// Read the per-browser UI selection. Separate from the server-side data so
// two tabs can watch different projects without fighting over /state.
function loadSelection() {
  try {
    const raw = localStorage.getItem(SELECTION_KEY);
    if (!raw) return { selectedProjectId: null, selectedSceneId: null };
    const parsed = JSON.parse(raw);
    return {
      selectedProjectId: parsed?.selectedProjectId ?? null,
      selectedSceneId: parsed?.selectedSceneId ?? null,
    };
  } catch {
    return { selectedProjectId: null, selectedSceneId: null };
  }
}

// Single source of truth for the projects + scenes UI. Everything below
// reads or mutates this object; deep reactivity means callers see updates
// without wrapping ceremony.
//
// Starts empty; hydrateFromServer() (kicked off below) fills it in once the
// GET /state round-trip lands. The UI shows the "no project selected" hint
// during that brief gap on cold cache.
export const scenesState = $state({
  ...emptyState(),
  ...loadSelection(),
  // True after hydrateFromServer resolves — components that need to gate
  // rendering on real data can watch this. Most UI reads flow naturally
  // (empty arrays render empty).
  ready: false,
});

function dataSnapshot() {
  return JSON.stringify({
    projects: scenesState.projects,
    scenes: scenesState.scenes,
    characters: scenesState.characters,
  });
}

// Serialized snapshot of what the server currently holds. Primed with the
// initial (empty) scenesState so the very first firing of the auto-save
// effect sees a no-op diff and doesn't race the async hydrate with a
// spurious "PUT empty state" that would wipe server data. Kept in sync
// with the server: bumped after each successful PUT and after hydrate.
let serverSnapshot = dataSnapshot();

async function hydrateFromServer() {
  let serverState = null;
  try {
    serverState = await getAppState();
  } catch (e) {
    console.warn('failed to load /state; starting empty', e);
  }
  const normalized = normalizePayload(serverState);
  scenesState.projects = normalized.projects;
  scenesState.scenes = normalized.scenes;
  scenesState.characters = normalized.characters;

  // Validate the current selection against what we ended up with — if the
  // referenced project or scene isn't there anymore, drop back to safe
  // defaults instead of leaving the user staring at a broken picker.
  const validProjectIds = new Set(normalized.projects.map((p) => p.id));
  if (scenesState.selectedProjectId && !validProjectIds.has(scenesState.selectedProjectId)) {
    scenesState.selectedProjectId = normalized.projects[0]?.id ?? null;
  }
  if (scenesState.selectedSceneId) {
    const scene = normalized.scenes.find((s) => s.id === scenesState.selectedSceneId);
    if (!scene || scene.projectId !== scenesState.selectedProjectId) {
      scenesState.selectedSceneId = null;
    }
  }
  scenesState.ready = true;

  // Prime the diff baseline so the auto-save $effect skips the free PUT of
  // the state we just GET'd back.
  serverSnapshot = dataSnapshot();
}

async function saveNow(serialized) {
  try {
    await putAppState(JSON.parse(serialized));
    serverSnapshot = serialized;
  } catch (e) {
    console.warn('failed to persist /state', e);
  }
}

let saveTimer = 0;
let pendingSerialized = null;
function scheduleSave(serialized) {
  pendingSerialized = serialized;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = 0;
    const payload = pendingSerialized;
    pendingSerialized = null;
    if (payload != null) saveNow(payload);
  }, STATE_SAVE_DEBOUNCE_MS);
}

$effect.root(() => {
  // Server-side data: debounce so a burst of edits sends one PUT, and skip
  // entirely if the diff against the last-known-server-state is a no-op.
  $effect(() => {
    // Explicitly read every slice we care about so Svelte tracks them.
    void scenesState.projects;
    void scenesState.scenes;
    void scenesState.characters;
    const current = dataSnapshot();
    if (current === serverSnapshot) return;
    scheduleSave(current);
  });

  // Local-only UI selection: write immediately, no network in the loop.
  $effect(() => {
    const sel = {
      selectedProjectId: scenesState.selectedProjectId,
      selectedSceneId: scenesState.selectedSceneId,
    };
    try { localStorage.setItem(SELECTION_KEY, JSON.stringify(sel)); } catch {}
  });
});

// Kick the hydration once the module loads. Fire-and-forget — components
// that render before it resolves see the empty seed above.
hydrateFromServer();

// ---- Project CRUD ----------------------------------------------------------

export function createProject(name = 'Untitled project') {
  const project = { id: uuid(), name };
  scenesState.projects.push(project);
  scenesState.selectedProjectId = project.id;
  scenesState.selectedSceneId = null;
  return project.id;
}

export function selectProject(id) {
  const exists = scenesState.projects.some((p) => p.id === id);
  scenesState.selectedProjectId = exists ? id : null;
  // Clear scene selection if it doesn't belong to the newly-selected project.
  if (scenesState.selectedSceneId) {
    const scene = scenesState.scenes.find((s) => s.id === scenesState.selectedSceneId);
    if (!scene || scene.projectId !== scenesState.selectedProjectId) {
      scenesState.selectedSceneId = null;
    }
  }
}

export function renameProject(id, name) {
  const project = scenesState.projects.find((p) => p.id === id);
  if (project) project.name = name;
}

/**
 * Cascade: delete every widget in every scene in this project on the server,
 * then drop the project + its scenes from client state. Returns `{ failed }`
 * — a count of widget deletes that errored. Local state is removed either
 * way, so callers can surface partial failures without leaving zombies.
 */
export async function deleteProject(id) {
  const idx = scenesState.projects.findIndex((p) => p.id === id);
  if (idx < 0) return { failed: 0 };
  const scenesInProject = scenesState.scenes.filter((s) => s.projectId === id);
  const charsInProject = scenesState.characters.filter((c) => c.projectId === id);
  const [widgetResults, imageResults] = await Promise.all([
    Promise.all(scenesInProject.map(cascadeDeleteSceneWidgets)),
    Promise.all(charsInProject.map(cascadeDeleteCharacterImages)),
  ]);
  const failed = widgetResults.reduce((n, r) => n + r.failed, 0)
               + imageResults.reduce((n, r) => n + r.failed, 0);
  // Re-lookup — state could have shifted while awaiting the network.
  const projectIdx = scenesState.projects.findIndex((p) => p.id === id);
  if (projectIdx >= 0) scenesState.projects.splice(projectIdx, 1);
  scenesState.scenes = scenesState.scenes.filter((s) => s.projectId !== id);
  scenesState.characters = scenesState.characters.filter((c) => c.projectId !== id);
  if (scenesState.selectedProjectId === id) {
    scenesState.selectedProjectId = scenesState.projects[0]?.id ?? null;
    scenesState.selectedSceneId = null;
  }
  return { failed };
}

export function currentProject() {
  return scenesState.selectedProjectId
    ? scenesState.projects.find((p) => p.id === scenesState.selectedProjectId) ?? null
    : null;
}

// ---- Scene CRUD ------------------------------------------------------------

export function createScene(name = 'Untitled scene') {
  const projectId = scenesState.selectedProjectId;
  if (!projectId) return null;
  const scene = {
    id: uuid(),
    name,
    projectId,
    lanes: [newLane('Voice 1')],
    clips: [],
    pauseMs: DEFAULT_PAUSE_MS,
  };
  scenesState.scenes.push(scene);
  scenesState.selectedSceneId = scene.id;
  return scene.id;
}

export function setScenePause(sceneId, pauseMs) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (scene) scene.pauseMs = Math.max(0, Math.round(pauseMs) || 0);
}

export function selectScene(id) {
  scenesState.selectedSceneId = scenesState.scenes.some((x) => x.id === id) ? id : null;
}

export function renameScene(id, name) {
  const scene = scenesState.scenes.find((x) => x.id === id);
  if (scene) scene.name = name;
}

/**
 * Cascade: delete every rendered widget in the scene on the server, then
 * drop the scene from client state. Returns `{ failed }` (widget delete
 * failure count). Local state is removed either way.
 */
export async function deleteScene(id) {
  const scene = scenesState.scenes.find((x) => x.id === id);
  if (!scene) return { failed: 0 };
  const projectId = scene.projectId;
  const result = await cascadeDeleteSceneWidgets(scene);
  // Re-lookup — state could have shifted while awaiting the network.
  const idx = scenesState.scenes.findIndex((x) => x.id === id);
  if (idx >= 0) scenesState.scenes.splice(idx, 1);
  if (scenesState.selectedSceneId === id) {
    const sibling = scenesState.scenes.find((s) => s.projectId === projectId);
    scenesState.selectedSceneId = sibling?.id ?? null;
  }
  return result;
}

// Scenes filtered to the currently-selected project.
export function currentProjectScenes() {
  const pid = scenesState.selectedProjectId;
  if (!pid) return [];
  return scenesState.scenes.filter((s) => s.projectId === pid);
}

// ---- Lane CRUD -------------------------------------------------------------
//
// `lane.voice` holds the id of the character speaking in that lane. The field
// name is kept for storage continuity with older sessions; old free-text
// values (e.g. "Voice 1") won't resolve to any character and render as
// unassigned in the picker until the user selects a character.

function newLane(characterId = '') {
  return { id: uuid(), voice: characterId };
}

export function addLane(sceneId, characterId = '') {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return;
  // Auto-pick the first project character so new lanes land already-wired.
  const fallback = characterId
    || scenesState.characters.find((c) => c.projectId === scene.projectId)?.id
    || '';
  scene.lanes.push(newLane(fallback));
}

export function setLaneVoice(sceneId, laneId, characterId) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return;
  const lane = scene.lanes.find((l) => l.id === laneId);
  if (lane) lane.voice = characterId;
}

export function getCharacter(id) {
  if (!id) return null;
  return scenesState.characters.find((c) => c.id === id) ?? null;
}

// Characters available to lanes in this scene (i.e. its project's roster).
export function sceneCharacters(scene) {
  if (!scene) return [];
  return scenesState.characters.filter((c) => c.projectId === scene.projectId);
}

/**
 * Delete a lane. Refuses (returns { ok:false, reason }) if any clip in the
 * scene still points at this lane — user has to remove those clips first.
 */
export function deleteLane(sceneId, laneId) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return { ok: false, reason: 'scene not found' };
  const idx = scene.lanes.findIndex((l) => l.id === laneId);
  if (idx < 0) return { ok: false, reason: 'lane not found' };
  if (scene.clips.some((c) => c.laneId === laneId)) {
    return { ok: false, reason: 'lane has clips — delete them first' };
  }
  scene.lanes.splice(idx, 1);
  return { ok: true };
}

export function laneHasClips(scene, laneId) {
  return scene.clips.some((c) => c.laneId === laneId);
}

// ---- Clip CRUD -------------------------------------------------------------

/**
 * Insert a new clip into this scene's timeline at `position`
 * (0 = before all existing clips, scene.clips.length = after all). The
 * clip is tagged with the given lane so it renders in that column.
 * Positions outside the valid range are clamped.
 */
export function insertClip(sceneId, laneId, position) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return null;
  if (!scene.lanes.some((l) => l.id === laneId)) return null;
  const id = uuid();
  const clamped = Math.max(0, Math.min(position | 0, scene.clips.length));
  scene.clips.splice(clamped, 0, { id, laneId, widgetId: null, text: '', styleId: null });
  return id;
}

/** Convenience: append at the end. */
export function addClip(sceneId, laneId) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return null;
  return insertClip(sceneId, laneId, scene.clips.length);
}

export function updateClip(sceneId, clipId, patch) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return;
  const clip = scene.clips.find((c) => c.id === clipId);
  if (!clip) return;
  Object.assign(clip, patch);
}

export function deleteClip(sceneId, clipId) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return;
  const idx = scene.clips.findIndex((c) => c.id === clipId);
  if (idx >= 0) scene.clips.splice(idx, 1);
}

// ---- Character CRUD --------------------------------------------------------

export function createCharacter(name = 'New character') {
  const projectId = scenesState.selectedProjectId;
  if (!projectId) return null;
  const character = {
    id: uuid(),
    projectId,
    name,
    voice: defaultVoice(),
    avatar: null,
    pictures: [],
  };
  scenesState.characters.push(character);
  return character.id;
}

export function updateCharacter(id, patch) {
  const character = scenesState.characters.find((c) => c.id === id);
  if (!character) return;
  if (patch.name !== undefined) character.name = patch.name;
  if (patch.avatar !== undefined) {
    // Swapping (or clearing) an avatar orphans the previous server-hosted
    // image — fire the delete in the background. Legacy dataURL avatars
    // have no image id and skip this branch.
    const priorId = imageIdFromSrc(character.avatar);
    const nextId  = imageIdFromSrc(patch.avatar);
    if (priorId && priorId !== nextId) {
      deleteImage(priorId).catch((e) => console.warn('avatar image cleanup failed', e));
    }
    character.avatar = patch.avatar;
  }
  if (patch.voice) {
    // Only merge scalar voice fields — style array edits go through the
    // dedicated addCharacterStyle/renameCharacterStyle/setCharacterStyleInstruct
    // helpers so we never accidentally clobber the whole list.
    if (patch.voice.speaker !== undefined) character.voice.speaker = patch.voice.speaker;
    if (patch.voice.language !== undefined) character.voice.language = patch.voice.language;
  }
}

// ---- Character voice styles -----------------------------------------------
//
// Every character owns >=1 style; the first is the fallback "default" used
// when a clip has no explicit selection. Deletes are refused if a style is
// the last one so the resolver never has to invent an empty style.

export function addCharacterStyle(characterId, name = 'new style') {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return null;
  const style = makeStyle(name, '');
  character.voice.styles.push(style);
  return style.id;
}

export function renameCharacterStyle(characterId, styleId, name) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return;
  const style = character.voice.styles.find((s) => s.id === styleId);
  if (style) style.name = name;
}

export function setCharacterStyleInstruct(characterId, styleId, instruct) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return;
  const style = character.voice.styles.find((s) => s.id === styleId);
  if (style) style.instruct = instruct;
}

/** Patch pitch/time on a style. Values outside the allowed range are clamped
 *  (rather than rejected) so slider drags and typed input both behave. */
export function setCharacterStyleEffect(characterId, styleId, patch) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return;
  const style = character.voice.styles.find((s) => s.id === styleId);
  if (!style) return;
  if (patch.pitchSemitones !== undefined) {
    style.pitchSemitones = clampNumber(
      patch.pitchSemitones,
      0,
      STYLE_PITCH_RANGE.min,
      STYLE_PITCH_RANGE.max,
    );
  }
  if (patch.timeRatio !== undefined) {
    style.timeRatio = clampNumber(
      patch.timeRatio,
      1,
      STYLE_TIME_RANGE.min,
      STYLE_TIME_RANGE.max,
    );
  }
}

export function deleteCharacterStyle(characterId, styleId) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return { ok: false, reason: 'character not found' };
  if (character.voice.styles.length <= 1) {
    return { ok: false, reason: 'at least one style is required' };
  }
  const idx = character.voice.styles.findIndex((s) => s.id === styleId);
  if (idx < 0) return { ok: false, reason: 'style not found' };
  character.voice.styles.splice(idx, 1);
  return { ok: true };
}

/**
 * Effective style for a clip: the character-owned style whose id matches
 * `clip.styleId`, or (legacy fallback) a synthetic style carrying the clip's
 * own persisted `instruct` field, or the character's first style. Callers
 * pass a character (may be null); the return is
 * `{ name, instruct, pitchSemitones, timeRatio }` — id is omitted because
 * the synthetic fallback has none. Fallbacks default to identity effects.
 */
export function resolveClipStyle(character, clip) {
  const empty = { name: '', instruct: '', pitchSemitones: 0, timeRatio: 1 };
  if (!character) return empty;
  const styles = character.voice.styles || [];
  const pick = (s) => ({
    name: s.name,
    instruct: s.instruct,
    pitchSemitones: Number.isFinite(s.pitchSemitones) ? s.pitchSemitones : 0,
    timeRatio:      Number.isFinite(s.timeRatio)      ? s.timeRatio      : 1,
  });
  if (clip?.styleId) {
    const match = styles.find((s) => s.id === clip.styleId);
    if (match) return pick(match);
  }
  // Pre-styles clips may still carry a per-clip instruct override. Honor it
  // so old scenes replay identically until the user re-picks a style.
  if (!clip?.styleId && typeof clip?.instruct === 'string' && clip.instruct !== '') {
    return { ...empty, instruct: clip.instruct };
  }
  const first = styles[0];
  return first ? pick(first) : empty;
}

export function deleteCharacter(id) {
  const idx = scenesState.characters.findIndex((c) => c.id === id);
  if (idx < 0) return;
  const character = scenesState.characters[idx];
  // Fire the image cascade in the background so the UI removal doesn't
  // block on the network — the DELETE is idempotent on the server.
  cascadeDeleteCharacterImages(character).catch((e) =>
    console.warn('character image cleanup failed', e),
  );
  scenesState.characters.splice(idx, 1);
}

export function addCharacterPicture(id, dataUrl, name = '') {
  const character = scenesState.characters.find((c) => c.id === id);
  if (!character) return;
  character.pictures.push({ id: uuid(), dataUrl, name });
}

export function removeCharacterPicture(characterId, pictureId) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return;
  const idx = character.pictures.findIndex((p) => p.id === pictureId);
  if (idx < 0) return;
  const picture = character.pictures[idx];
  const imageId = imageIdFromSrc(picture?.dataUrl);
  if (imageId) {
    deleteImage(imageId).catch((e) => console.warn('picture image cleanup failed', e));
  }
  character.pictures.splice(idx, 1);
}

export function currentProjectCharacters() {
  const pid = scenesState.selectedProjectId;
  if (!pid) return [];
  return scenesState.characters.filter((c) => c.projectId === pid);
}
