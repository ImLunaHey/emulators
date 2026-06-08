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
  // Box-art URL candidates to try in order. First successful load wins;
  // last entry is the placeholder fallback signal (empty string).
  thumbnails: string[];
}

const KEY_PREFIX = 'gba-recomp:hasheous:';

function readCache(md5: string): HasheousMeta | null | undefined {
  try {
    const raw = localStorage.getItem(KEY_PREFIX + md5);
    if (!raw) return undefined;
    if (raw === 'null') return null;
    return JSON.parse(raw) as HasheousMeta;
  } catch {
    return undefined;
  }
}
function writeCache(md5: string, meta: HasheousMeta | null): void {
  try {
    localStorage.setItem(KEY_PREFIX + md5, meta === null ? 'null' : JSON.stringify(meta));
  } catch { /* quota */ }
}

export async function lookupByMd5(md5: string): Promise<HasheousMeta | null> {
  const cached = readCache(md5);
  if (cached !== undefined) return cached;
  try {
    const r = await fetch(`/api/hasheous/lookup/byhash/md5/${md5}`);
    if (r.status === 404) {
      writeCache(md5, null);
      return null;
    }
    if (!r.ok) return null;
    const body = await r.json() as Record<string, unknown>;
    const meta = parseHasheousBody(body);
    writeCache(md5, meta);
    return meta;
  } catch {
    return null;
  }
}

function parseHasheousBody(body: Record<string, unknown>): HasheousMeta {
  const platform = (body.platform as Record<string, unknown> | undefined);
  const sig = body.signature as Record<string, unknown> | undefined;
  const game = sig?.game as Record<string, unknown> | undefined;
  const attrs = body.attributes as Array<Record<string, unknown>> | undefined;

  const name = (game?.name as string) ?? (body.name as string) ?? null;
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

  const thumbnails = name && platformName ? buildThumbnailUrls(name, platformName, region) : [];

  return { name, platform: platformName, publisher, year, region, description, thumbnails };
}

// Construct the URL candidates we'll try, in order, against the
// LibRetro thumbnails CDN (free + public + no auth). LibRetro names
// follow the No-Intro convention <Title> (<Regions>) [+ flags].png.
// Hasheous gives us the canonical title and at least one region, so we
// guess: name + region first, then name + USA, then name + Europe,
// then name + World, then name alone.
function buildThumbnailUrls(name: string, platform: string, region: string | null): string[] {
  if (!platform.toLowerCase().includes('boy advance')) return [];
  const system = 'Nintendo - Game Boy Advance';
  const enc = (s: string) =>
    encodeURIComponent(s)
      .replace(/%20/g, '%20')
      .replace(/'/g, '%27');
  const base = `https://thumbnails.libretro.com/${encodeURIComponent(system)}/Named_Boxarts/`;
  const variants: string[] = [];
  const tryFile = (rom: string) => variants.push(base + enc(rom) + '.png');
  if (region) tryFile(`${name} (${region})`);
  tryFile(`${name} (USA)`);
  tryFile(`${name} (USA, Europe)`);
  tryFile(`${name} (Europe)`);
  tryFile(`${name} (World)`);
  tryFile(`${name} (Japan)`);
  tryFile(name);
  // Dedupe while preserving order.
  return Array.from(new Set(variants));
}
