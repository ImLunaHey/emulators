// Build a compact GBA cheat index from the libretro-database submodule.
//
//   npm run gen:cheats
//
// Reads every `.cht` under the GBA folder (RetroArch format:
// `cheatN_desc = "..."`, `cheatN_code = "A+B+C+D"`), converts the
// `+`-joined hex words into our newline-separated "ADDR VALUE" cheat
// lines, and writes public/cheats-gba.json keyed by a normalized game
// name. The committed JSON is what the app ships — the 800 MB submodule
// is only needed to regenerate it, never at build/deploy time.

import { readdirSync, readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const SRC = 'vendor/libretro-database/cht/Nintendo - Game Boy Advance';
const OUT = 'apps/web/public/cheats-gba.json';

// Normalize a title for fuzzy matching: drop "(...)" region/rev groups,
// keep only lowercase alphanumerics. "Pokemon - FireRed Version (USA,
// Europe) (Rev 1)" → "pokemonfiredredversion"-ish; the ROM's metadata
// name normalizes the same way.
function normalize(name: string): string {
  return name.replace(/\([^)]*\)/g, '').toLowerCase().replace(/[^a-z0-9]/g, '');
}

// "C4000604+00008401+45525042" → "C4000604 00008401\n45525042" (pairs).
function convertCode(raw: string): string {
  const toks = raw.split('+').map((t) => t.trim()).filter(Boolean);
  const lines: string[] = [];
  for (let i = 0; i < toks.length; i += 2) {
    lines.push(toks[i + 1] ? `${toks[i]} ${toks[i + 1]}` : toks[i]);
  }
  return lines.join('\n');
}

// Some libretro descriptions are doubly/triply mojibaked (UTF-8 bytes
// repeatedly mis-decoded as Latin-1), e.g. "PokÃÂ©mon". Undo it by
// re-interpreting the string's bytes as UTF-8 until it stops changing;
// the U+FFFD guard means correctly-encoded text is left untouched.
function fixMojibake(s: string): string {
  for (let i = 0; i < 4; i++) {
    if (!/[-ÿ]/.test(s)) break;            // no Latin-1 high chars left
    let fixed: string;
    try { fixed = Buffer.from(s, 'latin1').toString('utf8'); } catch { break; }
    if (fixed === s || fixed.includes('�')) break; // over-decoded → stop
    s = fixed;
  }
  return s;
}

interface Entry { name: string; cheats: Array<{ name: string; code: string }>; }

if (!existsSync(SRC)) {
  console.error(`Source not found: ${SRC}\nRun: git submodule update --init --depth 1`);
  process.exit(1);
}

const index: Record<string, Entry> = {};
let files = 0, total = 0;

for (const file of readdirSync(SRC)) {
  if (!file.endsWith('.cht')) continue;
  files++;
  const text = readFileSync(join(SRC, file), 'utf8');
  const cheats: Array<{ name: string; code: string }> = [];
  for (let i = 0; ; i++) {
    const code = text.match(new RegExp(`cheat${i}_code\\s*=\\s*"([^"]*)"`));
    if (!code) break;
    const desc = text.match(new RegExp(`cheat${i}_desc\\s*=\\s*"([^"]*)"`));
    const conv = convertCode(code[1]);
    // Keep only real cheats: hex words + whitespace. Drops note/doc
    // pseudo-entries (e.g. code "N/A", "Important Note (").
    if (!/^[0-9A-Fa-f\s]+$/.test(conv) || !/[0-9A-Fa-f]{4,}/.test(conv)) continue;
    cheats.push({ name: fixMojibake((desc?.[1] || `Cheat ${i}`).trim()), code: conv });
  }
  if (cheats.length === 0) continue;
  const title = file.replace(/\.cht$/, '');
  const key = normalize(title);
  // On collisions (Rev 0/1, region variants) keep the richer set.
  if (!index[key] || index[key].cheats.length < cheats.length) {
    index[key] = { name: fixMojibake(title.replace(/\s*\([^)]*\)/g, '').trim()), cheats };
  }
  total += cheats.length;
}

writeFileSync(OUT, JSON.stringify(index));
console.log(`Wrote ${OUT}: ${Object.keys(index).length} games, ${total} cheats from ${files} files.`);
