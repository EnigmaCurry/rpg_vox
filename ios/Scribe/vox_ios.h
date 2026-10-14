// C ABI of crates/vox_ios. Handles are opaque; strings it returns are
// freed with vox_string_free. A function that can fail returns null or
// false and, when err isn't null, sets *err to the reason.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct VoxModels VoxModels;
typedef struct VoxSession VoxSession;
typedef struct VoxPlayer VoxPlayer;

// Extracts entries of a .tar.bz2 into dest; spec has one
// "<path below the top folder>\t<installed name>" per line. Blocks.
bool vox_unpack(const char *archive, const char *dest, const char *spec, char **err);
// Compressed bytes the running vox_unpack has read.
uint64_t vox_unpack_progress(void);

// Parakeet + Zipformer from dir. Slow (seconds).
VoxModels *vox_load(const char *dir, int32_t threads, char **err);
void vox_free_models(VoxModels *models);

// A session for rate Hz mono float samples; streaming gives live
// partials. With opus_path (may be null) the audio is also kept there.
VoxSession *vox_start(const VoxModels *models, uint32_t rate, bool streaming,
                      const char *opus_path, char **err);
void vox_push(const VoxSession *session, const float *samples, size_t len);
void vox_break_paragraph(const VoxSession *session);
// JSON array of events, waiting up to timeout_ms for the first.
char *vox_poll(const VoxSession *session, int32_t timeout_ms);
// Closes the recording and drains every pass; JSON {"paragraphs": [...]}.
// vox_poll and vox_push stay safe meanwhile; vox_free after.
char *vox_finish(const VoxSession *session);
void vox_free(VoxSession *session);

void vox_string_free(char *s);

// Timings for the words of text, an edit of the text words_json timed
// (and the same JSON back), within start_ms..end_ms. Null on bad JSON.
char *vox_align_words(const char *words_json, const char *text, uint64_t start_ms, uint64_t end_ms);

// An Ogg Opus recording for playback, at its start: mono float, 48 kHz.
VoxPlayer *vox_player_open(const char *path, char **err);
// First to last 50 ms louder than threshold_db dBFS, padded by pad_ms;
// false if none is. Rewinds the player.
bool vox_player_span(VoxPlayer *player, float threshold_db, uint64_t pad_ms,
                     uint64_t *start_ms, uint64_t *end_ms);
uint64_t vox_player_length(const VoxPlayer *player);
bool vox_player_seek(VoxPlayer *player, uint64_t ms);
// Samples written to out (at most len); fewer only at the end.
size_t vox_player_read(VoxPlayer *player, float *out, size_t len);
void vox_player_free(VoxPlayer *player);
