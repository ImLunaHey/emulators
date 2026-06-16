// Retro-console catalog. The launcher accepts ROMs for *all* of these; only
// the systems in PLAYABLE actually boot today (the rest show "coming soon").
// This is the host-side mirror of the eventual Rust `core_api::System` enum +
// `detect_system`; for now detection is by file extension.

export type SystemId =
  | 'gba' | 'nds' | 'gb' | 'gbc' | 'nes' | 'snes' | 'n64' | 'gg' | 'sms' | 'genesis' | 'ps1' | 'xbox'
  | 'pce' | 'atari2600' | 'ngpc' | 'wonderswan' | 'virtualboy';

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
  // PC Engine / TurboGrafx-16.
  pce: 'pce',
  // Atari 2600 (VCS).
  a26: 'atari2600',
  // Neo Geo Pocket (Color).
  ngp: 'ngpc', ngc: 'ngpc',
  // Bandai WonderSwan (Color).
  ws: 'wonderswan', wsc: 'wonderswan',
  // Nintendo Virtual Boy.
  vb: 'virtualboy', vboy: 'virtualboy',
};

export const SYSTEM_LABEL: Record<SystemId, string> = {
  gba: 'GBA', nds: 'NDS', gb: 'GB', gbc: 'GBC', nes: 'NES',
  snes: 'SNES', n64: 'N64', gg: 'GG', sms: 'SMS', genesis: 'GEN', ps1: 'PS1', xbox: 'XBOX',
  pce: 'PCE', atari2600: '2600', ngpc: 'NGPC', wonderswan: 'WS', virtualboy: 'VB',
};

// Per-console display metadata for the launcher's console grid. `accent` is the
// tile's signature color (a hex string used for the gradient + glow); `tagline`
// is a short descriptor shown under the name. The key order here also defines
// the order consoles appear on the home screen.
export interface SystemPresentation {
  accent: string;
  tagline: string;
}
export const SYSTEM_PRESENTATION: Record<SystemId, SystemPresentation> = {
  gba: { accent: '#7c5cff', tagline: 'Game Boy Advance' },
  nds: { accent: '#e0e0e6', tagline: 'Nintendo DS' },
  gb: { accent: '#9bbc0f', tagline: 'Game Boy' },
  gbc: { accent: '#ff5fa2', tagline: 'Game Boy Color' },
  snes: { accent: '#8b7fd4', tagline: 'Super Nintendo' },
  nes: { accent: '#e4000f', tagline: 'Nintendo' },
  n64: { accent: '#2e9e4f', tagline: 'Nintendo 64' },
  genesis: { accent: '#1a6dd6', tagline: 'Sega Genesis' },
  sms: { accent: '#e07b1f', tagline: 'Master System' },
  gg: { accent: '#1f7ae0', tagline: 'Game Gear' },
  pce: { accent: '#f2a900', tagline: 'PC Engine' },
  ps1: { accent: '#c9c9d4', tagline: 'PlayStation' },
  xbox: { accent: '#9cd530', tagline: 'Xbox' },
  atari2600: { accent: '#b8531f', tagline: 'Atari 2600' },
  ngpc: { accent: '#2bb7c4', tagline: 'Neo Geo Pocket' },
  wonderswan: { accent: '#3aa856', tagline: 'WonderSwan' },
  virtualboy: { accent: '#d4233b', tagline: 'Virtual Boy' },
};

/** Every system id, in the canonical home-screen display order. */
export const ALL_SYSTEMS: readonly SystemId[] = Object.keys(SYSTEM_PRESENTATION) as SystemId[];

export function systemPresentation(id: string): SystemPresentation {
  return SYSTEM_PRESENTATION[id as SystemId] ?? { accent: '#5fd0ff', tagline: systemLabel(id) };
}

// Systems with a working core. Everything else is addable but "coming soon".
// Xbox is a foundation core: it boots a supplied BIOS far enough to single-step
// x86 and shows a diagnostic crash screen, but does not run commercial games yet.
export const PLAYABLE: ReadonlySet<SystemId> = new Set<SystemId>([
  'gba', 'nds', 'nes', 'sms', 'gg', 'gbc', 'gb', 'ps1', 'xbox',
  'snes', 'genesis', 'n64', 'pce', 'atari2600', 'ngpc', 'wonderswan', 'virtualboy',
]);

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
