// Client-side store for Scene → Lane / Clip organization.
//
// Data shape (v2):
//   scene = {
//     id, name,
//     lanes: [{ id, voice }],            // columns; no clips owned here
//     clips: [{ id, laneId, widgetId, text, instruct }],  // ordered timeline
//   }
//
// Lanes are just column labels + a voice identity. Every clip belongs to a
// scene-level ordered list; its `laneId` picks which column it renders in.
// Vertical position in the UI = index in `scene.clips` = time. Two clips
// never share a "row" — voices take turns.
//
// v1 (older sessions) stored clips under lane.clips[]; the loader flattens
// those into the scene-level list in first-seen order so we don't lose
// anyone's typing when the schema bumps.
//
// Individual clip audio + text/instruct still live server-side under
// /widgets/{id}; this module only owns the *organization*. Uses Svelte 5
// `$state` (hence the `.svelte.js` extension) so in-place mutations
// propagate through the reactivity graph without callers needing to do
// immutable spreads. Persisted to localStorage on every change via a
// $effect.root so scenes survive reloads.

const STORAGE_KEY = 'rpg-vox:scenes:v1';   // key retained for continuity

function uuid() {
  if (typeof crypto !== 'undefined' && crypto.randomUUID) return crypto.randomUUID();
  return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    const v = c === 'x' ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

function emptyState() {
  return { scenes: [], selectedSceneId: null };
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
    // Already v2 — sanity-scrub in case an older bug left orphaned laneIds.
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
    return {
      scenes: parsed.scenes,
      selectedSceneId: parsed.selectedSceneId ?? null,
    };
  } catch {
    return emptyState();
  }
}

// Single source of truth for the scenes UI. Everything below reads or
// mutates this object; deep reactivity means callers see updates without
// wrapping ceremony.
export const scenesState = $state(load());

$effect.root(() => {
  $effect(() => {
    try {
      const snapshot = JSON.stringify({
        scenes: scenesState.scenes,
        selectedSceneId: scenesState.selectedSceneId,
      });
      localStorage.setItem(STORAGE_KEY, snapshot);
    } catch {
      // localStorage full or blocked — in-memory state still works.
    }
  });
});

// ---- Scene CRUD ------------------------------------------------------------

export function createScene(name = 'Untitled scene') {
  const scene = {
    id: uuid(),
    name,
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

export function deleteScene(id) {
  const idx = scenesState.scenes.findIndex((x) => x.id === id);
  if (idx < 0) return;
  scenesState.scenes.splice(idx, 1);
  if (scenesState.selectedSceneId === id) {
    scenesState.selectedSceneId = scenesState.scenes[0]?.id ?? null;
  }
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
