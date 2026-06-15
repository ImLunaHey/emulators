// Retro-console catalog. The launcher accepts ROMs for *all* of these; only
// the systems in PLAYABLE actually boot today (the rest show "coming soon").
// This is the host-side mirror of the eventual Rust `core_api::System` enum +
// `detect_system`; for now detection is by file extension.

export type SystemId =
  | 'gba' | 'nds' | 'gb' | 'gbc' | 'nes' | 'snes' | 'n64' | 'gg' | 'sms' | 'genesis' | 'ps1' | 'xbox';

const EXT_TO_SYSTEM: Record<string, SystemId> = {
  gba: 'gba',
  nds: 'nds',
  gb: 'gb',
  gbc: 'gbc',
  nes: 'nes',
  smc: 'snes', sfc: 'snes',
  n64: 'n64', z64: 'n64', v64: 'n64',
  gg: 'gg',
  sms: 'sms',
  md: 'genesis', gen: 'genesis', smd: 'genesis',
  // PS1 disc images. .bin/.cue come in a pair (the .cue indexes the .bin); the
  // .bin holds the data. (.bin is genericish — Genesis uses .md/.gen here, so
  // mapping it to PS1 is fine for this catalog.)
  cue: 'ps1', bin: 'ps1', img: 'ps1', iso: 'ps1', pbp: 'ps1',
  // Original Xbox. .xbe is the executable; .xiso is the redump disc image. (.iso
  // is already claimed by PS1 above; Xbox discs are typically dumped as .xiso.)
  xbe: 'xbox', xiso: 'xbox',
};

export const SYSTEM_LABEL: Record<SystemId, string> = {
  gba: 'GBA', nds: 'NDS', gb: 'GB', gbc: 'GBC', nes: 'NES',
  snes: 'SNES', n64: 'N64', gg: 'GG', sms: 'SMS', genesis: 'GEN', ps1: 'PS1', xbox: 'XBOX',
};

// Systems with a working core. Everything else is addable but "coming soon".
// Xbox is a foundation core: it boots a supplied BIOS far enough to single-step
// x86 and shows a diagnostic crash screen, but does not run commercial games yet.
export const PLAYABLE: ReadonlySet<SystemId> = new Set<SystemId>(['gba', 'nds', 'nes', 'sms', 'gg', 'gbc', 'gb', 'ps1', 'xbox']);

/** Detect a system from a filename's extension, or null if unrecognized. */
export function detectSystem(filename: string): SystemId | null {
  const m = filename.toLowerCase().match(/\.([a-z0-9]+)$/);
  return m ? EXT_TO_SYSTEM[m[1]] ?? null : null;
}

export function systemLabel(id: string): string {
  return SYSTEM_LABEL[id as SystemId] ?? id.toUpperCase();
}

export function isPlayable(id: string): boolean {
  return PLAYABLE.has(id as SystemId);
}

/** `accept` attribute for the add-game file input (every ROM ext + .zip). */
export const ACCEPT = [...Object.keys(EXT_TO_SYSTEM).map((e) => `.${e}`), '.zip'].join(',');
