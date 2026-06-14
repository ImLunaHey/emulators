// Retro-console catalog. The launcher accepts ROMs for *all* of these; only
// the systems in PLAYABLE actually boot today (the rest show "coming soon").
// This is the host-side mirror of the eventual Rust `core_api::System` enum +
// `detect_system`; for now detection is by file extension.

export type SystemId =
  | 'gba' | 'nds' | 'gb' | 'gbc' | 'nes' | 'snes' | 'n64' | 'gg' | 'sms' | 'genesis';

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
};

export const SYSTEM_LABEL: Record<SystemId, string> = {
  gba: 'GBA', nds: 'NDS', gb: 'GB', gbc: 'GBC', nes: 'NES',
  snes: 'SNES', n64: 'N64', gg: 'GG', sms: 'SMS', genesis: 'GEN',
};

// Systems with a working core. Everything else is addable but "coming soon".
export const PLAYABLE: ReadonlySet<SystemId> = new Set<SystemId>(['gba', 'nds']);

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
