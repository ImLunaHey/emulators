/*
 * emu_native.h — unified C ABI for every emulator core (GBA/PS1/NDS/NES/SMS+GG/
 * GBC/Xbox). Implemented by the `emu-native` Rust crate (../src/lib.rs), built as
 * a static archive and linked into native front-ends — currently the macOS
 * SwiftPM app (apps/EmuApp). A future libretro adapter wraps the same functions.
 *
 * Contract:
 *   - emu_new() once per session; emu_free() to release.
 *   - per frame: emu_set_keys(), emu_run_frame(), then read the framebuffer via
 *     emu_framebuffer_ptr()/_len() (RGBA8888, emu_width()*emu_height()*4). The
 *     pointer is refreshed by emu_run_frame(); copy/upload before the next call.
 *   - audio: emu_drain_audio() copies interleaved samples; pair with
 *     emu_sample_rate()/emu_channels().
 *
 * NOTE for a libretro port: the framebuffer is RGBA8888 (byte order R,G,B,A);
 * libretro XRGB8888 expects B,G,R,A, so swizzle in the video adapter. Audio is
 * f32 here; libretro wants i16 — convert in the adapter.
 */
#ifndef EMU_NATIVE_H
#define EMU_NATIVE_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* System selector for emu_new(). Keep in sync with the Rust `System` enum. */
typedef enum {
    EMU_SYSTEM_GBA = 0,
    EMU_SYSTEM_PS1 = 1,
    EMU_SYSTEM_NDS = 2,
    EMU_SYSTEM_NES = 3,
    EMU_SYSTEM_SMS = 4,
    EMU_SYSTEM_GAME_GEAR = 5,
    EMU_SYSTEM_GBC = 6,
    EMU_SYSTEM_XBOX = 7
} EmuSystem;

/* Opaque session handle. */
typedef struct Emu Emu;

/* Allocate a core for `system`. Returns NULL on an unknown id. */
Emu *emu_new(uint32_t system);

/* Release a handle from emu_new(). */
void emu_free(Emu *emu);

/* Load a ROM / disc image. Returns true on success. */
bool emu_load_rom(Emu *emu, const uint8_t *data, size_t len);

/* Load a BIOS / flash image (PS1, Xbox). Returns true if the core used it. */
bool emu_load_bios(Emu *emu, const uint8_t *data, size_t len);

/* Run one frame and refresh the framebuffer. */
void emu_run_frame(Emu *emu);

/* Set the controller button bitmask. */
void emu_set_keys(Emu *emu, uint32_t bits);

/* Current RGBA8888 framebuffer (valid until the next emu_run_frame/emu_free). */
const uint8_t *emu_framebuffer_ptr(const Emu *emu);
size_t emu_framebuffer_len(const Emu *emu);

/* Current display size in pixels. */
uint32_t emu_width(const Emu *emu);
uint32_t emu_height(const Emu *emu);

/* Drain interleaved audio into `out` (capacity `max` floats). Returns count. */
size_t emu_drain_audio(Emu *emu, float *out, size_t max);
uint32_t emu_sample_rate(const Emu *emu);
uint32_t emu_channels(const Emu *emu);

/* Frames completed since reset. */
uint32_t emu_frame_count(const Emu *emu);

#ifdef __cplusplus
}
#endif

#endif /* EMU_NATIVE_H */
