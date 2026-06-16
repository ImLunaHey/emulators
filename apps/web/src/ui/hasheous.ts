// Hasheous client. Hashes a ROM byte buffer (MD5 via small inline
// implementation — SubtleCrypto omits MD5), looks it up through our
// /api/hasheous Cloudflare Worker proxy (Hasheous omits CORS headers),
// and returns the fields we care about for the library UI.

export async function md5Hex(bytes: Uint8Array): Promise<string> {
  // RFC 1321 MD5. Straight transcription with no optimization.
  // Returns a 32-character lowercase hex string.
  const r = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
  ];
  const k = new Uint32Array(64);
  for (let i = 0; i < 64; i++) {
    k[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 0x100000000) >>> 0;
  }

  const bitLen = BigInt(bytes.length) * 8n;
  const padLen = ((bytes.length + 9 + 63) & ~63) - bytes.length;
  const buf = new Uint8Array(bytes.length + padLen);
  buf.set(bytes);
  buf[bytes.length] = 0x80;
  const bl = new DataView(buf.buffer, buf.length - 8);
  bl.setUint32(0, Number(bitLen & 0xFFFFFFFFn), true);
  bl.setUint32(4, Number((bitLen >> 32n) & 0xFFFFFFFFn), true);

  let a0 = 0x67452301, b0 = 0xefcdab89, c0 = 0x98badcfe, d0 = 0x10325476;
  const m = new Uint32Array(16);
  const view = new DataView(buf.buffer);
  for (let off = 0; off < buf.length; off += 64) {
    for (let i = 0; i < 16; i++) m[i] = view.getUint32(off + i * 4, true);
    let a = a0, b = b0, c = c0, d = d0;
    for (let i = 0; i < 64; i++) {
      let f: number, g: number;
      if (i < 16)      { f = (b & c) | (~b & d);              g = i; }
      else if (i < 32) { f = (d & b) | (~d & c);              g = (5 * i + 1) & 15; }
      else if (i < 48) { f = b ^ c ^ d;                       g = (3 * i + 5) & 15; }
      else             { f = c ^ (b | ~d);                    g = (7 * i) & 15; }
      const temp = d;
      d = c;
      c = b;
      const sum = (a + f + k[i] + m[g]) >>> 0;
      const rot = r[i];
      b = (b + ((sum << rot) | (sum >>> (32 - rot)))) >>> 0;
      a = temp;
    }
    a0 = (a0 + a) >>> 0;
    b0 = (b0 + b) >>> 0;
    c0 = (c0 + c) >>> 0;
    d0 = (d0 + d) >>> 0;
  }
  const out = new Uint8Array(16);
  const ov = new DataView(out.buffer);
  ov.setUint32(0, a0, true);
  ov.setUint32(4, b0, true);
  ov.setUint32(8, c0, true);
  ov.setUint32(12, d0, true);
  let hex = '';
  for (const v of out) hex += v.toString(16).padStart(2, '0');
  return hex;
}

// Normalized metadata we surface to the UI.
export interface HasheousMeta {
  name: string | null;        // canonical title — "Pokemon - FireRed Version"
  platform: string | null;    // "Nintendo Game Boy Advance"
  publisher: string | null;   // "Nintendo"
  year: string | null;        // "2004" (string, not int — Hasheous sometimes has ranges)
  region: string | null;      // first known region code: "USA", "Europe", "Japan", etc.
  description: string | null; // long-form blurb (AI-generated or curated)
  // IGDB game ID — used by the /api/igdb/cover/<id> worker endpoint
  // to fetch real box art. Null when Hasheous didn't map this hash to
  // an IGDB entry.
  igdbId: number | null;
  // Box-art URL candidates to try in order. First successful load wins;
  // last entry is the placeholder fallback signal (empty string).
  thumbnails: string[];
}

// Bump when HasheousMeta's shape changes — old cache entries from
// previous schema (no `thumbnails` array, etc.) will be ignored. We
// sweep older versions out of storage at module load so they don't
// occupy quota indefinitely.
// Sweep any stale localStorage keys from the pre-TanStack era so they
// don't sit forever in quota. The new caching layer lives in
// queryClient.ts and persists under 'emulators:rq:v1'.
(() => {
  try {
    const stale: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && (
        k.startsWith('emulators:hasheous:') ||
        k.startsWith('emulators:cover:') ||
        k === 'emulators:rq:v1' ||
        k === 'emulators:rq:v2'
      )) stale.push(k);
    }
    for (const k of stale) localStorage.removeItem(k);
    for (let i = 0; i < sessionStorage.length; i++) {
      const k = sessionStorage.key(i);
      if (k && k.startsWith('emulators:cover:')) sessionStorage.removeItem(k);
    }
  } catch { /* private mode or storage disabled */ }
})();

// Single network round-trip — no local caching anymore. The
// queryClient in queryClient.ts handles in-memory + localStorage
// persistence for everything that comes through useHasheousMeta.
export async function lookupByMd5(md5: string): Promise<HasheousMeta | null> {
  const r = await fetch(`/api/hasheous/lookup/byhash/md5/${md5}`);
  if (r.status === 404) return null;
  if (!r.ok) return null;
  const body = await r.json() as Record<string, unknown>;
  return parseHasheousBody(body);
}

function parseHasheousBody(body: Record<string, unknown>): HasheousMeta {
  const platform = (body.platform as Record<string, unknown> | undefined);
  const sig = body.signature as Record<string, unknown> | undefined;
  const game = sig?.game as Record<string, unknown> | undefined;
  const attrs = body.attributes as Array<Record<string, unknown>> | undefined;

  // Prefer body.name (ASCII canonical) over signature.game.name when
  // the latter has diacritics like "Pokémon" — LibRetro filenames are
  // all plain ASCII and won't match the Unicode variant.
  const rawName = (body.name as string) ?? (game?.name as string) ?? null;
  // Strip combining diacritics. Hasheous sometimes returns "Pokémon"
  // (Unicode) where LibRetro filenames are plain ASCII "Pokemon".
  // ̀-ͯ is the Combining Diacritical Marks block.
  const name = rawName ? rawName.normalize('NFD').replace(/[̀-ͯ]/g, '') : null;
  const platformName = (platform?.name as string) ?? null;
  const publisher = (game?.publisher as string) ?? (body.publisher as string) ?? null;
  const year = (game?.year as string) ?? null;
  const countries = game?.countries as Record<string, string> | undefined;
  const region = countries ? Object.values(countries)[0] ?? null : null;

  // Pull description from the attributes block — Hasheous lists per-source
  // entries; "AIDescription" is the most reliably populated.
  let description: string | null = null;
  if (attrs) {
    const desc = attrs.find((a) => a.attributeName === 'AIDescription' || a.attributeName === 'Description');
    if (desc) description = (desc.value as string) ?? null;
  }

  // Find the IGDB game id from the metadata array. The lookup
  // response has a nested object that lists every metadata source
  // and the id assigned to this game under each. IGDB ids are short
  // numerics; we parse to int and ignore non-numeric values.
  let igdbId: number | null = null;
  const metas = body.metadata as Array<Record<string, unknown>> | undefined;
  if (metas) {
    const igdb = metas.find((m) => m.source === 'IGDB');
    const raw = igdb?.id;
    if (typeof raw === 'string' && /^\d+$/.test(raw)) igdbId = parseInt(raw, 10);
    else if (typeof raw === 'number') igdbId = raw;
  }

  const thumbnails = name && platformName ? buildThumbnailUrls(name, platformName, region) : [];

  return { name, platform: platformName, publisher, year, region, description, igdbId, thumbnails };
}

// Construct candidate URLs to try against the LibRetro thumbnails
// CDN (public, no auth). LibRetro follows the No-Intro convention:
// <Title> (<Region>) [(<Languages>)] [<Flags>].png. Hasheous gives us
// the canonical title and usually a region, so we guess a generous
// set of common variants — including European multi-language suffixes
// like "(Europe) (En,Fr,De,Es,It)" that Garfield-style PAL releases use.
function buildThumbnailUrls(name: string, platform: string, region: string | null): string[] {
  if (!platform.toLowerCase().includes('boy advance')) return [];
  const system = 'Nintendo - Game Boy Advance';
  const base = `https://thumbnails.libretro.com/${encodeURIComponent(system)}/Named_Boxarts/`;
  const enc = (s: string) => encodeURIComponent(s).replace(/'/g, '%27');
  const variants: string[] = [];
  const tryFile = (rom: string) => variants.push(base + enc(rom) + '.png');
  if (region) tryFile(`${name} (${region})`);
  tryFile(`${name} (USA, Europe)`);
  tryFile(`${name} (USA)`);
  tryFile(`${name} (USA) (En,Fr,De,Es,It)`);
  tryFile(`${name} (USA) (En,Fr,Es)`);
  tryFile(`${name} (Europe)`);
  tryFile(`${name} (Europe) (En,Fr,De,Es,It)`);
  tryFile(`${name} (Europe) (En,Fr,De,It)`);
  tryFile(`${name} (Europe) (En,Fr,De,Es,It,Nl)`);
  tryFile(`${name} (Europe) (En,Es,It)`);
  tryFile(`${name} (Europe) (En,Fr,De)`);
  tryFile(`${name} (World)`);
  tryFile(`${name} (Japan)`);
  tryFile(name);
  return Array.from(new Set(variants));
}
