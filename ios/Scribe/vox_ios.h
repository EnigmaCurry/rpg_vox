// C ABI of crates/vox_ios. Handles are opaque; strings it returns are
// freed with vox_string_free.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct VoxModels VoxModels;
typedef struct VoxSession VoxSession;

// Parakeet + Zipformer from dir; null with *err set on failure.
VoxModels *vox_load(const char *dir, int32_t threads, char **err);
void vox_free_models(VoxModels *models);

// A session for rate Hz mono float samples; streaming gives live partials.
VoxSession *vox_start(const VoxModels *models, uint32_t rate, bool streaming);
void vox_push(const VoxSession *session, const float *samples, size_t len);
void vox_break_paragraph(const VoxSession *session);
// JSON array of events, waiting up to timeout_ms for the first.
char *vox_poll(const VoxSession *session, int32_t timeout_ms);
// Drains every pass; JSON {"paragraphs": [...]}.
char *vox_finish(const VoxSession *session);
void vox_free(VoxSession *session);

void vox_string_free(char *s);
