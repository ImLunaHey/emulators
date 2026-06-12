// ROM files live under public/ but are gitignored (copyrighted + large),
// so they're absent in CI. Tests that need a real ROM should gate on
// availability with vitest's skipIf so CI stays green:
//
//   import { hasRom, readRom } from './roms';
//   const ROM = romPath('firered.gba');
//   describe.skipIf(!hasRom(ROM))('…', () => { const bytes = readRom(ROM); … });

import { existsSync, readFileSync } from 'node:fs';

export const romPath = (name: string): string => `public/${name}`;
export const hasRom = (path: string): boolean => existsSync(path);
export const readRom = (path: string): Uint8Array => new Uint8Array(readFileSync(path));
