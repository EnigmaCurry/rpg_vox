// Client-side store for Project → Scene → Lane / Clip organization,
// plus per-project Characters.
//
// Data shape (v3):
//   project   = { id, name }
//   character = {
//     id, projectId, name,
//     voice: { speaker, language, instruct },
//     avatar: dataUrl | null,
//     pictures: [{ id, dataUrl, name }],
//   }
//   scene     = {
//     id, name, projectId,
//     lanes: [{ id, voice }],            // columns; no clips owned here
//     clips: [{ id, laneId, widgetId, text, instruct }],  // ordered timeline
//   }
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
// Uses Svelte 5
// `$state` (hence the `.svelte.js` extension) so in-place mutations
// propagate through the reactivity graph without callers needing to do
// immutable spreads. Persisted to localStorage on every change via a
// $effect.root so state survives reloads.

import { deleteWidget } from './api.js';

const STORAGE_KEY = 'rpg-vox:scenes:v1';   // key retained for continuity

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

function defaultVoice() {
  return { speaker: QWEN3_SPEAKERS[0], language: QWEN3_LANGUAGES[0], instruct: '' };
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
          instruct: c.instruct ?? '',
        });
      }
      delete lane.clips;
    }
  } else {
    // Already v2+ — sanity-scrub in case an older bug left orphaned laneIds.
    const validLaneIds = new Set(scene.lanes.map((l) => l.id));
    scene.clips = scene.clips.filter((c) => validLaneIds.has(c.laneId));
  }
  if (typeof scene.pauseMs !== 'number') scene.pauseMs = DEFAULT_PAUSE_MS;
}

export const DEFAULT_PAUSE_MS = 300;
export const PAUSE_OPTIONS = [0, 150, 300, 500, 1000, 2000];

function load() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return emptyState();
    const parsed = JSON.parse(raw);
    if (!parsed || !Array.isArray(parsed.scenes)) return emptyState();
    for (const scene of parsed.scenes) migrateScene(scene);

    let projects = Array.isArray(parsed.projects) ? parsed.projects : [];
    let selectedProjectId = parsed.selectedProjectId ?? null;

    // v2 → v3: any scene missing projectId gets adopted by a Default project.
    const orphans = parsed.scenes.filter((s) => !s.projectId);
    if (orphans.length > 0) {
      let defaultProject = projects.find((p) => p.name === 'Default');
      if (!defaultProject) {
        defaultProject = { id: uuid(), name: 'Default' };
        projects.push(defaultProject);
      }
      for (const s of orphans) s.projectId = defaultProject.id;
      if (!selectedProjectId) selectedProjectId = defaultProject.id;
    }

    // Drop any scene pointing at a project that no longer exists.
    const validProjectIds = new Set(projects.map((p) => p.id));
    const scenes = parsed.scenes.filter((s) => validProjectIds.has(s.projectId));

    // Keep selectedProjectId honest.
    if (selectedProjectId && !validProjectIds.has(selectedProjectId)) {
      selectedProjectId = projects[0]?.id ?? null;
    }

    // Keep selectedSceneId honest — must be a scene in the current project.
    let selectedSceneId = parsed.selectedSceneId ?? null;
    if (selectedSceneId) {
      const scene = scenes.find((s) => s.id === selectedSceneId);
      if (!scene || scene.projectId !== selectedProjectId) selectedSceneId = null;
    }

    // Characters: drop any pointing at a project that no longer exists, and
    // backfill missing sub-fields so older sessions load without null checks.
    let characters = Array.isArray(parsed.characters) ? parsed.characters : [];
    characters = characters
      .filter((c) => c && validProjectIds.has(c.projectId))
      .map((c) => ({
        id: c.id ?? uuid(),
        projectId: c.projectId,
        name: c.name ?? 'Unnamed',
        voice: {
          speaker: c.voice?.speaker ?? QWEN3_SPEAKERS[0],
          language: c.voice?.language ?? QWEN3_LANGUAGES[0],
          instruct: c.voice?.instruct ?? '',
        },
        avatar: typeof c.avatar === 'string' ? c.avatar : null,
        pictures: Array.isArray(c.pictures)
          ? c.pictures.filter((p) => p && typeof p.dataUrl === 'string').map((p) => ({
              id: p.id ?? uuid(),
              dataUrl: p.dataUrl,
              name: p.name ?? '',
            }))
          : [],
      }));

    return { projects, selectedProjectId, scenes, selectedSceneId, characters };
  } catch {
    return emptyState();
  }
}

// Single source of truth for the projects + scenes UI. Everything below
// reads or mutates this object; deep reactivity means callers see updates
// without wrapping ceremony.
export const scenesState = $state(load());

$effect.root(() => {
  $effect(() => {
    try {
      const snapshot = JSON.stringify({
        projects: scenesState.projects,
        selectedProjectId: scenesState.selectedProjectId,
        scenes: scenesState.scenes,
        selectedSceneId: scenesState.selectedSceneId,
        characters: scenesState.characters,
      });
      localStorage.setItem(STORAGE_KEY, snapshot);
    } catch {
      // localStorage full or blocked — in-memory state still works.
    }
  });
});

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
  const results = await Promise.all(scenesInProject.map(cascadeDeleteSceneWidgets));
  const failed = results.reduce((n, r) => n + r.failed, 0);
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

function newLane(voice = '') {
  return { id: uuid(), voice };
}

export function addLane(sceneId, voice = '') {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return;
  const nextIdx = scene.lanes.length + 1;
  scene.lanes.push(newLane(voice || `Voice ${nextIdx}`));
}

export function setLaneVoice(sceneId, laneId, voice) {
  const scene = scenesState.scenes.find((x) => x.id === sceneId);
  if (!scene) return;
  const lane = scene.lanes.find((l) => l.id === laneId);
  if (lane) lane.voice = voice;
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
  scene.clips.splice(clamped, 0, { id, laneId, widgetId: null, text: '', instruct: '' });
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
  if (patch.avatar !== undefined) character.avatar = patch.avatar;
  if (patch.voice) Object.assign(character.voice, patch.voice);
}

export function deleteCharacter(id) {
  const idx = scenesState.characters.findIndex((c) => c.id === id);
  if (idx >= 0) scenesState.characters.splice(idx, 1);
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
  if (idx >= 0) character.pictures.splice(idx, 1);
}

export function currentProjectCharacters() {
  const pid = scenesState.selectedProjectId;
  if (!pid) return [];
  return scenesState.characters.filter((c) => c.projectId === pid);
}
