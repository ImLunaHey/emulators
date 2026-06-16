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
    EMU_SYSTEM_XBOX = 7,
    EMU_SYSTEM_SNES = 8,
    EMU_SYSTEM_GENESIS = 9,
    EMU_SYSTEM_PCE = 10,
    EMU_SYSTEM_ATARI2600 = 11,
    EMU_SYSTEM_NGPC = 12,
    EMU_SYSTEM_WONDERSWAN = 13,
    EMU_SYSTEM_VIRTUALBOY = 14,
    EMU_SYSTEM_N64 = 15
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

/* Set the controller button bitmask (per-system native layout). */
void emu_set_keys(Emu *emu, uint32_t bits);

/* Logical (RetroPad-style) button bits for emu_set_buttons. Face buttons use
 * the position->letter abstraction: EAST=A (right), SOUTH=B (bottom),
 * NORTH=X (top), WEST=Y (left). */
#define EMU_BTN_UP     (1u << 0)
#define EMU_BTN_DOWN   (1u << 1)
#define EMU_BTN_LEFT   (1u << 2)
#define EMU_BTN_RIGHT  (1u << 3)
#define EMU_BTN_SOUTH  (1u << 4)
#define EMU_BTN_EAST   (1u << 5)
#define EMU_BTN_WEST   (1u << 6)
#define EMU_BTN_NORTH  (1u << 7)
#define EMU_BTN_L1     (1u << 8)
#define EMU_BTN_R1     (1u << 9)
#define EMU_BTN_L2     (1u << 10)
#define EMU_BTN_R2     (1u << 11)
#define EMU_BTN_START  (1u << 12)
#define EMU_BTN_SELECT (1u << 13)

/* Set controller state from a logical EMU_BTN_* mask, mapped per system. */
void emu_set_buttons(Emu *emu, uint32_t logical);

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

/* ---- saves / memory cards / HDD ---- */

/* On-disk save category for a system (pick the right extension + manager UI). */
typedef enum {
    EMU_SAVE_NONE = 0,        /* no persistent storage */
    EMU_SAVE_BATTERY = 1,     /* .sav (SRAM/Flash/EEPROM) */
    EMU_SAVE_MEMORY_CARD = 2, /* .mcr (PS1) */
    EMU_SAVE_HDD = 3          /* raw HDD image (Xbox) */
} EmuSaveKind;

/* Save category for `system` (does not need a live handle). */
uint32_t emu_save_kind(uint32_t system);

/* Byte length of the current save image (0 if the core has no battery store). */
size_t emu_save_data_len(const Emu *emu);

/* Copy the save image into `out` (capacity `max`). Returns bytes written. */
size_t emu_save_data(const Emu *emu, uint8_t *out, size_t max);

/* Load a .sav image into the core's battery store (after emu_load_rom). */
void emu_load_save(Emu *emu, const uint8_t *data, size_t len);

/* Whether the save store changed since the last emu_clear_save_dirty. */
bool emu_save_dirty(const Emu *emu);

/* Clear the save-dirty flag (call after persisting the .sav). */
void emu_clear_save_dirty(Emu *emu);

/* ---- link attachments ---- */

/* Attachment kinds + the supported-bitmask flags (1 << kind). */
typedef enum {
    EMU_ATTACH_NONE = 0,
    EMU_ATTACH_LINK_CABLE = 1,
    EMU_ATTACH_WIRELESS_ADAPTER = 2
} EmuAttachment;
#define EMU_ATTACH_FLAG_LINK_CABLE (1u << 1)
#define EMU_ATTACH_FLAG_WIRELESS_ADAPTER (1u << 2)

/* Bitmask of attachments `system` supports (EMU_ATTACH_FLAG_*); 0 if none. */
uint32_t emu_supported_attachments(uint32_t system);

/* Select the active link attachment (EmuAttachment). No-op if unsupported. */
void emu_set_attachment(Emu *emu, uint32_t attachment);

#ifdef __cplusplus
}
#endif

#endif /* EMU_NATIVE_H */
