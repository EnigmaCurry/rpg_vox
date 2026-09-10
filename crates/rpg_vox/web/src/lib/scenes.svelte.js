// Client-side store for Project → Scene → Lane / Clip organization,
// plus per-project Characters.
//
// Data shape (v5):
//   project   = {
//     id, name,
//     dictionary: [{ id, word, pronunciation }],  // TTS pronunciation proxies
//   }
//   character = {
//     id, projectId, name,
//     voiceProfiles: [{                    // >=1; first is the fallback "default"
//       id, name,
//       configs: [{                        // >=1; N>1 = layered hive-mind synth
//         id,
//         mode,                            // 'presets' | 'clone' | 'design' | 'copy'
//         // Preset-mode fields (unused in clone/design/copy):
//         speaker, instruct,               // Qwen3 preset-model inputs
//         // Design-mode field:
//         description,                     // free-text voice description
//         // Clone-mode field:
//         voiceFileId,                     // server-side /voices/{id} ref
//         // Copy-mode field:
//         copyFromIndex,                   // 0-based index of the source voice
//         // Sample-mode field:
//         sampleFileId,                    // server-side /samples/{id} ref
//         // Common across all modes:
//         language,                        // lang_disp for every Qwen3 endpoint
//         pitchSemitones, timeRatio,       // post-synthesis DSP (per-config)
//         detuneCents,                     // fine pitch offset (added to semitones)
//         pan,                             // -1 = full L, +1 = full R (equal-power)
//         gainDb,                          // per-voice level in dB
//         delayMs,                         // start offset in the final mix
//         hpfHz, lpfHz,                    // one-pole bandpass (0 = off, either side)
//         driveDb,                         // tanh saturation drive in dB (0 = off)
//         crushBits,                       // bit-crush quantization depth (0 = off)
//         amRateHz, amDepth,               // amplitude-mod / tremolo (rate 0 = off)
//         ringHz, ringMix,                 // true ring mod, `x·sin(2πf t)` (hz 0 = off)
//         reverbMix,                       // Freeverb-lite wet/dry (0 = off)
//         reverbRoom, reverbDamp,          // reverb feedback + HF damping (0..1)
//         reverbTailMs,                    // fixed audible tail length ms (fade to 0)
//       }],
//     }],
//     avatar: dataUrl | null,
//     pictures: [{ id, dataUrl, name }],
//   }
//   scene     = {
//     id, name, projectId,
//     lanes: [{ id, voice }],              // columns; no clips owned here
//     clips: [{ id, laneId, widgetId, text, profileId }], // ordered timeline
//   }
//
// v3 sessions stored `character.voice = { speaker, language, styles[] }` with
// each style holding an instruct + pitch/time DSP knobs. v4 flattens that into
// `voiceProfiles[configs[]]`, letting a single profile fan out into multiple
// concurrent voices for hive-mind / crowd effects. On load, each v3 style
// becomes a v4 profile with one config carrying the character's old
// speaker/language plus the style's instruct/pitch/time (new fields default to
// pan=0, gainDb=0, detuneCents=0, delayMs=0). v2 sessions (single
// `voice.instruct` string) pass through the v3 promotion first.
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

// Coerce an unknown value into the canonical dictionary shape. Entries
// missing either field are dropped so the server never sees a half-populated
// row that would replace a word with nothing.
function sanitizeDictionary(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((e) => e && typeof e === 'object')
    .map((e) => ({
      id: typeof e.id === 'string' && e.id ? e.id : uuid(),
      word: typeof e.word === 'string' ? e.word : '',
      pronunciation: typeof e.pronunciation === 'string' ? e.pronunciation : '',
    }));
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
// config stays in sync. All-zero identity values (except `timeRatio = 1`)
// are what the server short-circuits on so an all-defaults config pays
// no DSP cost. Every knob whose range starts at 0 uses 0 as its "off"
// sentinel: HPF/LPF at 0 Hz are bypassed, driveDb=0 skips the saturator,
// crushBits=0 skips the quantizer, and amRateHz=0 or amDepth=0 skips the
// tremolo pass.
export const CONFIG_PITCH_RANGE       = { min: -24,  max: 24,   step: 1 };
export const CONFIG_TIME_RANGE        = { min: 0.25, max: 4,    step: 0.05 };
export const CONFIG_DETUNE_RANGE      = { min: -100, max: 100,  step: 1 };
export const CONFIG_PAN_RANGE         = { min: -1,   max: 1,    step: 0.05 };
export const CONFIG_GAIN_DB_RANGE     = { min: -60,  max: 12,   step: 0.5 };
export const CONFIG_DELAY_MS_RANGE    = { min: 0,    max: 5000, step: 10 };
export const CONFIG_HPF_RANGE         = { min: 0,    max: 2000, step: 10 };
export const CONFIG_LPF_RANGE         = { min: 0,    max: 20000, step: 100 };
export const CONFIG_DRIVE_DB_RANGE    = { min: 0,    max: 24,   step: 0.5 };
export const CONFIG_CRUSH_BITS_RANGE  = { min: 0,    max: 16,   step: 1 };
export const CONFIG_AM_RATE_RANGE     = { min: 0,    max: 200,  step: 1 };
export const CONFIG_AM_DEPTH_RANGE    = { min: 0,    max: 1,    step: 0.05 };
// Ring modulator: 0 disables. 30–80 Hz = Dalek, 200–600 Hz = clanky computer,
// 1–3 kHz = glassy inharmonic tinge. `ringMix` crossfades dry↔wet.
export const CONFIG_RING_HZ_RANGE     = { min: 0,    max: 3000, step: 1 };
export const CONFIG_RING_MIX_RANGE    = { min: 0,    max: 1,    step: 0.05 };
// Freeverb-lite reverb. `reverbMix` is wet/dry; `reverbRoom` maps to
// feedback in [0.7, 0.98]; `reverbDamp` is HF loss in the feedback loop.
// `reverbTailMs` is the fixed audible tail — the wet fades to 0 across this
// window regardless of `reverbRoom`, so every clip has the same outro shape.
export const CONFIG_REVERB_MIX_RANGE  = { min: 0,    max: 1,    step: 0.05 };
export const CONFIG_REVERB_ROOM_RANGE = { min: 0,    max: 1,    step: 0.05 };
export const CONFIG_REVERB_DAMP_RANGE = { min: 0,    max: 1,    step: 0.05 };
export const CONFIG_REVERB_TAIL_RANGE = { min: 0,    max: 3000, step: 50 };

function clampNumber(value, fallback, min, max) {
  const n = Number(value);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

// Enum of per-config voice modes.
// * `presets` / `clone` / `design` — synthesize speech via Qwen3, see
//   resolve_character_configs and tts::SynthMode.
// * `copy` — reuse another config's raw synth output; the copy still runs
//   its own pitch/time/FX/gain chain, so a Mechanicum-style pitched double
//   is just "copy voice 1 with pitch = -3".
// * `sample` — loop a user-uploaded audio clip for the profile's duration
//   and run the same FX chain on top. Used for machine drones, ambience,
//   any non-speech texture that layers under the voice.
export const VOICE_MODES = ['presets', 'clone', 'design', 'copy', 'sample'];
export const DEFAULT_VOICE_MODE = 'presets';

// Seed a single voice config with the given speaker/language and default
// (identity) effects. Callers that need a specific instruct/pitch/etc.
// override the returned fields directly. Mode-specific fields (description,
// voiceFileId) start empty; the UI populates them when the parent profile
// switches to a mode that uses them.
function makeConfig({
  mode = DEFAULT_VOICE_MODE,
  speaker = QWEN3_SPEAKERS[0],
  language = QWEN3_LANGUAGES[0],
  instruct = '',
  description = '',
  voiceFileId = null,
  sampleFileId = null,
  copyFromIndex = 0,
  pitchSemitones = 0,
  timeRatio = 1,
  detuneCents = 0,
  pan = 0,
  gainDb = 0,
  delayMs = 0,
  hpfHz = 0,
  lpfHz = 0,
  driveDb = 0,
  crushBits = 0,
  amRateHz = 0,
  amDepth = 0,
  ringHz = 0,
  ringMix = 1,
  reverbMix = 0,
  reverbRoom = 0.7,
  reverbDamp = 0.5,
  reverbTailMs = 500,
} = {}) {
  return {
    id: uuid(),
    mode: VOICE_MODES.includes(mode) ? mode : DEFAULT_VOICE_MODE,
    speaker,
    language,
    instruct,
    description,
    voiceFileId,
    sampleFileId,
    copyFromIndex,
    pitchSemitones,
    timeRatio,
    detuneCents,
    pan,
    gainDb,
    delayMs,
    hpfHz,
    lpfHz,
    driveDb,
    crushBits,
    amRateHz,
    amDepth,
    ringHz,
    ringMix,
    reverbMix,
    reverbRoom,
    reverbDamp,
    reverbTailMs,
  };
}

function makeVoiceProfile(name = 'default', configs = null) {
  return {
    id: uuid(),
    name,
    configs: configs && configs.length > 0 ? configs : [makeConfig()],
  };
}

function defaultVoiceProfiles() {
  return [makeVoiceProfile('default')];
}

// Coerce one config-shaped object into a fully-populated config with
// sanitized numeric fields. Missing fields fall back to identity values so
// an older or partial payload still loads cleanly.
//
// `defaultMode` is the mode a raw config falls back to when it doesn't
// carry its own `mode` field. Used during the v5→v6 migration to push
// the old profile-level mode into every config so legacy profiles keep
// synthesizing under their original mode without an explicit write.
function sanitizeConfig(raw, defaultMode = DEFAULT_VOICE_MODE) {
  const c = raw && typeof raw === 'object' ? raw : {};
  const mode = VOICE_MODES.includes(c.mode)
    ? c.mode
    : (VOICE_MODES.includes(defaultMode) ? defaultMode : DEFAULT_VOICE_MODE);
  // `copyFromIndex` is 0-based on the wire; only meaningful when mode is
  // 'copy'. We keep it around for other modes too so toggling into copy
  // doesn't reset the selection; it just gets ignored server-side.
  const rawCopyIdx = Number(c.copyFromIndex);
  const copyFromIndex = Number.isInteger(rawCopyIdx) && rawCopyIdx >= 0 ? rawCopyIdx : 0;
  return {
    id: typeof c.id === 'string' ? c.id : uuid(),
    mode,
    speaker: typeof c.speaker === 'string' && c.speaker ? c.speaker : QWEN3_SPEAKERS[0],
    language: typeof c.language === 'string' && c.language ? c.language : QWEN3_LANGUAGES[0],
    instruct: typeof c.instruct === 'string' ? c.instruct : '',
    // Mode-specific fields default to empty; migrating a legacy profile
    // (which was implicitly preset-mode) leaves these idle until the user
    // switches modes.
    description: typeof c.description === 'string' ? c.description : '',
    voiceFileId: typeof c.voiceFileId === 'string' && c.voiceFileId ? c.voiceFileId : null,
    sampleFileId: typeof c.sampleFileId === 'string' && c.sampleFileId ? c.sampleFileId : null,
    copyFromIndex,
    pitchSemitones:   clampNumber(c.pitchSemitones,   0, CONFIG_PITCH_RANGE.min,      CONFIG_PITCH_RANGE.max),
    timeRatio:        clampNumber(c.timeRatio,        1, CONFIG_TIME_RANGE.min,       CONFIG_TIME_RANGE.max),
    detuneCents:      clampNumber(c.detuneCents,      0, CONFIG_DETUNE_RANGE.min,     CONFIG_DETUNE_RANGE.max),
    pan:              clampNumber(c.pan,              0, CONFIG_PAN_RANGE.min,        CONFIG_PAN_RANGE.max),
    gainDb:           clampNumber(c.gainDb,           0, CONFIG_GAIN_DB_RANGE.min,    CONFIG_GAIN_DB_RANGE.max),
    delayMs:          clampNumber(c.delayMs,          0, CONFIG_DELAY_MS_RANGE.min,   CONFIG_DELAY_MS_RANGE.max),
    hpfHz:            clampNumber(c.hpfHz,            0, CONFIG_HPF_RANGE.min,        CONFIG_HPF_RANGE.max),
    lpfHz:            clampNumber(c.lpfHz,            0, CONFIG_LPF_RANGE.min,        CONFIG_LPF_RANGE.max),
    driveDb:          clampNumber(c.driveDb,          0, CONFIG_DRIVE_DB_RANGE.min,   CONFIG_DRIVE_DB_RANGE.max),
    crushBits:        clampNumber(c.crushBits,        0, CONFIG_CRUSH_BITS_RANGE.min, CONFIG_CRUSH_BITS_RANGE.max),
    amRateHz:         clampNumber(c.amRateHz,         0, CONFIG_AM_RATE_RANGE.min,    CONFIG_AM_RATE_RANGE.max),
    amDepth:          clampNumber(c.amDepth,          0, CONFIG_AM_DEPTH_RANGE.min,   CONFIG_AM_DEPTH_RANGE.max),
    ringHz:           clampNumber(c.ringHz,           0,   CONFIG_RING_HZ_RANGE.min,     CONFIG_RING_HZ_RANGE.max),
    ringMix:          clampNumber(c.ringMix,          1,   CONFIG_RING_MIX_RANGE.min,    CONFIG_RING_MIX_RANGE.max),
    reverbMix:        clampNumber(c.reverbMix,        0,   CONFIG_REVERB_MIX_RANGE.min,  CONFIG_REVERB_MIX_RANGE.max),
    reverbRoom:       clampNumber(c.reverbRoom,       0.7, CONFIG_REVERB_ROOM_RANGE.min, CONFIG_REVERB_ROOM_RANGE.max),
    reverbDamp:       clampNumber(c.reverbDamp,       0.5, CONFIG_REVERB_DAMP_RANGE.min, CONFIG_REVERB_DAMP_RANGE.max),
    reverbTailMs:     clampNumber(c.reverbTailMs,     500, CONFIG_REVERB_TAIL_RANGE.min, CONFIG_REVERB_TAIL_RANGE.max),
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
          // v3→v4: styleId is the same UUID as the v4 profile id (migration
          // in normalizePayload keeps ids stable).
          profileId: c.profileId ?? c.styleId ?? null,
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
      .map((c) => ({
        profileId: c.profileId ?? c.styleId ?? null,
        ...c,
        // Ensure profileId wins over any stale spread from styleId.
        ...(c.profileId ? { profileId: c.profileId } : {}),
      }))
      // Drop the deprecated styleId field so it doesn't leak back into
      // future PUT /state payloads.
      .map((c) => {
        const { styleId, ...rest } = c;
        return rest;
      });
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

  let projects = (Array.isArray(parsed.projects) ? parsed.projects : []).map((p) => ({
    ...p,
    dictionary: sanitizeDictionary(p?.dictionary),
  }));

  // v2 → v3: any scene missing projectId gets adopted by a Default project.
  const orphans = rawScenes.filter((s) => !s.projectId);
  if (orphans.length > 0) {
    let defaultProject = projects.find((p) => p.name === 'Default');
    if (!defaultProject) {
      defaultProject = { id: uuid(), name: 'Default', dictionary: [] };
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
      // v4+: already stores voiceProfiles[configs[]]. Sanitize each profile
      // and its configs so partial/older payloads land in a consistent shape.
      //
      // v5 → v6: mode used to live on the profile; now every config carries
      // its own. `sanitizeConfig(cc, p.mode)` seeds each config's `mode` from
      // the legacy profile-level field so an old preset/clone/design profile
      // renders identically after load without an explicit migration write.
      let voiceProfiles = Array.isArray(c.voiceProfiles)
        ? c.voiceProfiles
            .filter((p) => p && typeof p === 'object')
            .map((p) => {
              const legacyMode = VOICE_MODES.includes(p.mode) ? p.mode : DEFAULT_VOICE_MODE;
              const configs = Array.isArray(p.configs)
                ? p.configs
                    .filter((cc) => cc && typeof cc === 'object')
                    .map((cc) => sanitizeConfig(cc, legacyMode))
                : [];
              return {
                id: typeof p.id === 'string' ? p.id : uuid(),
                name: typeof p.name === 'string' && p.name ? p.name : 'unnamed',
                configs: configs.length > 0 ? configs : [makeConfig({ mode: legacyMode })],
              };
            })
        : null;
      // v3 → v4 migration: each style becomes a profile with one config
      // carrying the character's old speaker/language + the style's
      // instruct/pitch/time. New per-config knobs (detune/pan/gain/delay)
      // default to their identity values so migrated projects sound the
      // same as before.
      if (!voiceProfiles || voiceProfiles.length === 0) {
        const baseSpeaker = typeof c.voice?.speaker === 'string' && c.voice.speaker
          ? c.voice.speaker : QWEN3_SPEAKERS[0];
        const baseLanguage = typeof c.voice?.language === 'string' && c.voice.language
          ? c.voice.language : QWEN3_LANGUAGES[0];
        const oldStyles = Array.isArray(c.voice?.styles) ? c.voice.styles : [];
        if (oldStyles.length > 0) {
          voiceProfiles = oldStyles
            .filter((s) => s && typeof s === 'object')
            .map((s) => ({
              // Keep the style id as the profile id so any clip.styleId
              // still resolves against the migrated profile.
              id: typeof s.id === 'string' ? s.id : uuid(),
              name: typeof s.name === 'string' && s.name ? s.name : 'unnamed',
              configs: [sanitizeConfig({
                speaker: baseSpeaker,
                language: baseLanguage,
                instruct: s.instruct,
                pitchSemitones: s.pitchSemitones,
                timeRatio: s.timeRatio,
              })],
            }));
        } else {
          // v2 or fresh: seed a single default profile with one config,
          // promoting any legacy top-level voice.instruct into it.
          voiceProfiles = [makeVoiceProfile('default', [sanitizeConfig({
            speaker: baseSpeaker,
            language: baseLanguage,
            instruct: typeof c.voice?.instruct === 'string' ? c.voice.instruct : '',
          })])];
        }
      }
      return {
        id: c.id ?? uuid(),
        projectId: c.projectId,
        name: c.name ?? 'Unnamed',
        voiceProfiles,
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
  const project = { id: uuid(), name, dictionary: [] };
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
  scene.clips.splice(clamped, 0, { id, laneId, widgetId: null, text: '', profileId: null });
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
    voiceProfiles: defaultVoiceProfiles(),
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
  // Voice profile / config mutations go through the dedicated helpers below
  // so bulk edits never accidentally clobber the whole list.
}

// ---- Character voice profiles + configs ----------------------------------
//
// Every character owns >=1 profile; the first is the fallback "default" used
// when a clip has no explicit selection. Every profile owns >=1 config;
// synthesizing a profile with N>1 configs fans out into N concurrent Qwen3
// calls that are mixed together (per-config pan/gain/delay) so a single
// profile can voice a crowd/hive-mind. Deletes are refused when they'd take
// either list below its minimum so the resolver never sees an empty shape.

export function addVoiceProfile(characterId, name = 'new profile') {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return null;
  const profile = makeVoiceProfile(name);
  character.voiceProfiles.push(profile);
  return profile.id;
}

export function renameVoiceProfile(characterId, profileId, name) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return;
  const profile = character.voiceProfiles.find((p) => p.id === profileId);
  if (profile) profile.name = name;
}

export function deleteVoiceProfile(characterId, profileId) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return { ok: false, reason: 'character not found' };
  if (character.voiceProfiles.length <= 1) {
    return { ok: false, reason: 'at least one profile is required' };
  }
  const idx = character.voiceProfiles.findIndex((p) => p.id === profileId);
  if (idx < 0) return { ok: false, reason: 'profile not found' };
  character.voiceProfiles.splice(idx, 1);
  return { ok: true };
}

/** Append a new config to a profile. When the profile already has configs,
 *  the new one clones the last config's speaker/language/instruct so layering
 *  another voice onto a hive-mind is one click; effect knobs reset to identity
 *  so the added layer starts neutral. */
export function addProfileConfig(characterId, profileId) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return null;
  const profile = character.voiceProfiles.find((p) => p.id === profileId);
  if (!profile) return null;
  const seed = profile.configs[profile.configs.length - 1];
  const config = makeConfig(
    seed
      ? { speaker: seed.speaker, language: seed.language, instruct: seed.instruct }
      : {},
  );
  profile.configs.push(config);
  return config.id;
}

/** Patch any subset of a config's fields. Numeric values are clamped to the
 *  allowed range; strings are stored as-is. Missing fields are left alone. */
export function updateProfileConfig(characterId, profileId, configId, patch) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return;
  const profile = character.voiceProfiles.find((p) => p.id === profileId);
  if (!profile) return;
  const config = profile.configs.find((c) => c.id === configId);
  if (!config) return;
  if (patch.mode !== undefined && VOICE_MODES.includes(patch.mode)) {
    config.mode = patch.mode;
  }
  if (patch.speaker !== undefined) config.speaker = patch.speaker || QWEN3_SPEAKERS[0];
  if (patch.language !== undefined) config.language = patch.language || QWEN3_LANGUAGES[0];
  if (patch.instruct !== undefined) config.instruct = String(patch.instruct);
  if (patch.description !== undefined) config.description = String(patch.description);
  if (patch.voiceFileId !== undefined) {
    config.voiceFileId = typeof patch.voiceFileId === 'string' && patch.voiceFileId
      ? patch.voiceFileId
      : null;
  }
  if (patch.sampleFileId !== undefined) {
    config.sampleFileId = typeof patch.sampleFileId === 'string' && patch.sampleFileId
      ? patch.sampleFileId
      : null;
  }
  if (patch.copyFromIndex !== undefined) {
    const n = Number(patch.copyFromIndex);
    // Range check happens against the profile's actual config count at
    // synthesis time; here we only guard against negatives / NaN so the
    // stored value stays a valid array index candidate.
    config.copyFromIndex = Number.isInteger(n) && n >= 0 ? n : 0;
  }
  if (patch.pitchSemitones !== undefined) {
    config.pitchSemitones = clampNumber(patch.pitchSemitones, config.pitchSemitones,
      CONFIG_PITCH_RANGE.min, CONFIG_PITCH_RANGE.max);
  }
  if (patch.timeRatio !== undefined) {
    config.timeRatio = clampNumber(patch.timeRatio, config.timeRatio,
      CONFIG_TIME_RANGE.min, CONFIG_TIME_RANGE.max);
  }
  if (patch.detuneCents !== undefined) {
    config.detuneCents = clampNumber(patch.detuneCents, config.detuneCents,
      CONFIG_DETUNE_RANGE.min, CONFIG_DETUNE_RANGE.max);
  }
  if (patch.pan !== undefined) {
    config.pan = clampNumber(patch.pan, config.pan,
      CONFIG_PAN_RANGE.min, CONFIG_PAN_RANGE.max);
  }
  if (patch.gainDb !== undefined) {
    config.gainDb = clampNumber(patch.gainDb, config.gainDb,
      CONFIG_GAIN_DB_RANGE.min, CONFIG_GAIN_DB_RANGE.max);
  }
  if (patch.delayMs !== undefined) {
    config.delayMs = clampNumber(patch.delayMs, config.delayMs,
      CONFIG_DELAY_MS_RANGE.min, CONFIG_DELAY_MS_RANGE.max);
  }
  if (patch.hpfHz !== undefined) {
    config.hpfHz = clampNumber(patch.hpfHz, config.hpfHz,
      CONFIG_HPF_RANGE.min, CONFIG_HPF_RANGE.max);
  }
  if (patch.lpfHz !== undefined) {
    config.lpfHz = clampNumber(patch.lpfHz, config.lpfHz,
      CONFIG_LPF_RANGE.min, CONFIG_LPF_RANGE.max);
  }
  if (patch.driveDb !== undefined) {
    config.driveDb = clampNumber(patch.driveDb, config.driveDb,
      CONFIG_DRIVE_DB_RANGE.min, CONFIG_DRIVE_DB_RANGE.max);
  }
  if (patch.crushBits !== undefined) {
    config.crushBits = clampNumber(patch.crushBits, config.crushBits,
      CONFIG_CRUSH_BITS_RANGE.min, CONFIG_CRUSH_BITS_RANGE.max);
  }
  if (patch.amRateHz !== undefined) {
    config.amRateHz = clampNumber(patch.amRateHz, config.amRateHz,
      CONFIG_AM_RATE_RANGE.min, CONFIG_AM_RATE_RANGE.max);
  }
  if (patch.amDepth !== undefined) {
    config.amDepth = clampNumber(patch.amDepth, config.amDepth,
      CONFIG_AM_DEPTH_RANGE.min, CONFIG_AM_DEPTH_RANGE.max);
  }
  if (patch.ringHz !== undefined) {
    config.ringHz = clampNumber(patch.ringHz, config.ringHz,
      CONFIG_RING_HZ_RANGE.min, CONFIG_RING_HZ_RANGE.max);
  }
  if (patch.ringMix !== undefined) {
    config.ringMix = clampNumber(patch.ringMix, config.ringMix,
      CONFIG_RING_MIX_RANGE.min, CONFIG_RING_MIX_RANGE.max);
  }
  if (patch.reverbMix !== undefined) {
    config.reverbMix = clampNumber(patch.reverbMix, config.reverbMix,
      CONFIG_REVERB_MIX_RANGE.min, CONFIG_REVERB_MIX_RANGE.max);
  }
  if (patch.reverbRoom !== undefined) {
    config.reverbRoom = clampNumber(patch.reverbRoom, config.reverbRoom,
      CONFIG_REVERB_ROOM_RANGE.min, CONFIG_REVERB_ROOM_RANGE.max);
  }
  if (patch.reverbDamp !== undefined) {
    config.reverbDamp = clampNumber(patch.reverbDamp, config.reverbDamp,
      CONFIG_REVERB_DAMP_RANGE.min, CONFIG_REVERB_DAMP_RANGE.max);
  }
  if (patch.reverbTailMs !== undefined) {
    config.reverbTailMs = clampNumber(patch.reverbTailMs, config.reverbTailMs,
      CONFIG_REVERB_TAIL_RANGE.min, CONFIG_REVERB_TAIL_RANGE.max);
  }
}

export function deleteProfileConfig(characterId, profileId, configId) {
  const character = scenesState.characters.find((c) => c.id === characterId);
  if (!character) return { ok: false, reason: 'character not found' };
  const profile = character.voiceProfiles.find((p) => p.id === profileId);
  if (!profile) return { ok: false, reason: 'profile not found' };
  if (profile.configs.length <= 1) {
    return { ok: false, reason: 'at least one config is required' };
  }
  const idx = profile.configs.findIndex((c) => c.id === configId);
  if (idx < 0) return { ok: false, reason: 'config not found' };
  profile.configs.splice(idx, 1);
  return { ok: true };
}

/**
 * Effective voice profile for a clip: the character-owned profile whose id
 * matches `clip.profileId`, or the character's first profile as a fallback.
 * Legacy `clip.instruct` overrides (pre-styles era) are honored by returning
 * a synthetic single-config profile that carries only the instruct string.
 * Returns `{ name, configs: [...] }` — id is omitted because synthetic
 * fallbacks have none. Configs come back sanitized so callers can trust the
 * numeric ranges.
 */
export function resolveClipProfile(character, clip) {
  const empty = { name: '', configs: [sanitizeConfig()] };
  if (!character) return empty;
  const profiles = character.voiceProfiles || [];
  const pick = (p) => ({
    name: p.name,
    configs: (p.configs || []).map(sanitizeConfig),
  });
  if (clip?.profileId) {
    const match = profiles.find((p) => p.id === clip.profileId);
    if (match) return pick(match);
  }
  // Pre-styles clips may still carry a per-clip instruct override. Honor it
  // so old scenes replay identically until the user re-picks a profile.
  if (!clip?.profileId && typeof clip?.instruct === 'string' && clip.instruct !== '') {
    return { name: '', configs: [sanitizeConfig({ instruct: clip.instruct })] };
  }
  const first = profiles[0];
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

// ---- Project dictionary (TTS pronunciation proxies) ----------------------
//
// Each project carries a list of `{ word, pronunciation }` pairs. The server
// applies these as case-insensitive whole-word substitutions to any text
// bound for the TTS backend, so a mispronounced word like "Omnisiah" can be
// re-spelled phonetically ("OmniSighYa") for synthesis without leaking that
// respelling into any text sent to the LLM. Storage rides along in the
// existing project JSON (persisted via PUT /state).

function projectById(id) {
  return scenesState.projects.find((p) => p.id === id) ?? null;
}

function ensureDictionary(project) {
  if (!Array.isArray(project.dictionary)) project.dictionary = [];
  return project.dictionary;
}

export function currentProjectDictionary() {
  const project = projectById(scenesState.selectedProjectId);
  if (!project) return [];
  return ensureDictionary(project);
}

export function addDictionaryEntry(projectId, word = '', pronunciation = '') {
  const project = projectById(projectId);
  if (!project) return null;
  const entry = { id: uuid(), word, pronunciation };
  ensureDictionary(project).push(entry);
  return entry.id;
}

export function updateDictionaryEntry(projectId, entryId, patch) {
  const project = projectById(projectId);
  if (!project) return;
  const entry = ensureDictionary(project).find((e) => e.id === entryId);
  if (!entry) return;
  if (patch.word !== undefined) entry.word = String(patch.word);
  if (patch.pronunciation !== undefined) entry.pronunciation = String(patch.pronunciation);
}

export function removeDictionaryEntry(projectId, entryId) {
  const project = projectById(projectId);
  if (!project) return;
  const dict = ensureDictionary(project);
  const idx = dict.findIndex((e) => e.id === entryId);
  if (idx >= 0) dict.splice(idx, 1);
}
