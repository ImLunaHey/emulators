/*
 * gba_core_ffi.h — C ABI for the GBA core (React Native / mobile target).
 *
 * Hand-maintained to match core-ffi/src/lib.rs. The opaque `GbaCore` handle
 * wraps `gba_core::Gba`. See lib.rs for the per-function safety contract; in
 * short: one handle per session, pointers borrowed for the call only, and the
 * framebuffer/save pointers alias core memory (copy out before the next frame).
 */
#ifndef GBA_CORE_FFI_H
#define GBA_CORE_FFI_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct GbaCore GbaCore;

/* lifecycle */
GbaCore *gba_new(void);
void gba_free(GbaCore *gba);

/* cartridge + run loop */
void gba_load_rom(GbaCore *gba, const uint8_t *data, size_t len);
void gba_run_frame(GbaCore *gba);
void gba_set_keys(GbaCore *gba, uint32_t bits);
uint32_t gba_frame_count(const GbaCore *gba);

/* video: zero-copy 240x160 RGBA8888 (len = 153600). Valid until next frame. */
const uint8_t *gba_framebuffer_ptr(const GbaCore *gba);
size_t gba_framebuffer_len(const GbaCore *gba);

/* audio: interleaved-stereo f32, drained into caller buffer (returns count). */
size_t gba_drain_audio(GbaCore *gba, float *out, size_t max);

/* battery save */
size_t gba_save_ram(const GbaCore *gba, uint8_t *out, size_t max);
void gba_load_save_ram(GbaCore *gba, const uint8_t *data, size_t len);
bool gba_save_dirty(const GbaCore *gba);
void gba_clear_save_dirty(GbaCore *gba);

#ifdef __cplusplus
}
#endif

#endif /* GBA_CORE_FFI_H */
