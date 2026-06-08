// Hasheous client. Hashes a ROM byte buffer (MD5 via SubtleCrypto) and
// looks it up through our /api/hasheous worker proxy. The proxy adds
// the missing CORS header and edge-caches results.
//
// Hasheous's response is a large nested object pulling metadata from
// IGDB / TheGamesDb / GiantBomb; we narrow it down to the few fields
// the library UI actually uses.

// SubtleCrypto.digest gives SHA-1/256/384/512, not MD5 — and we want
// MD5 because Hasheous's catalog is keyed primarily off MD5 hashes
// from the No-Intro datfiles. Implement a small MD5 routine inline
// rather than pulling a dependency.
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

  // Pad to 64-byte blocks: 0x80, then zeros, then bit-length (LE 64).
  const bitLen = BigInt(bytes.length) * 8n;
  const padLen = ((bytes.length + 9 + 63) & ~63) - bytes.length;
  const buf = new Uint8Array(bytes.length + padLen);
  buf.set(bytes);
  buf[bytes.length] = 0x80;
  // Little-endian 64-bit bit-length at the end.
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

// The bits of Hasheous's response we care about for library display.
export interface HasheousMeta {
  name: string | null;
  platform: string | null;
  releaseDate: string | null;
  coverUrl: string | null;
  // Raw response if a caller wants more.
  raw: unknown;
}

// In-memory + localStorage cache so we don't re-query Hasheous for
// every library list render. Keyed by md5.
const KEY_PREFIX = 'gba-recomp:hasheous:';

function readCache(md5: string): HasheousMeta | null | undefined {
  try {
    const raw = localStorage.getItem(KEY_PREFIX + md5);
    if (!raw) return undefined;            // never queried
    if (raw === 'null') return null;       // queried, no match
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
    const meta: HasheousMeta = {
      name: (body.name as string) ?? null,
      platform: ((body.platform as Record<string, unknown> | undefined)?.name as string) ?? null,
      releaseDate: extractReleaseDate(body),
      coverUrl: extractCoverUrl(body),
      raw: body,
    };
    writeCache(md5, meta);
    return meta;
  } catch {
    // Network/proxy errors — don't cache so a retry on next session
    // can try again.
    return null;
  }
}

function extractReleaseDate(body: Record<string, unknown>): string | null {
  // Hasheous bundles platform metadata + signatures. The first matching
  // release date in body.signatures[0].game.releasedate is the usual one.
  const sigs = body.signatures as Array<Record<string, unknown>> | undefined;
  if (!sigs || sigs.length === 0) return null;
  const game = sigs[0].game as Record<string, unknown> | undefined;
  const date = game?.releasedate as string | undefined;
  return date ?? null;
}

function extractCoverUrl(body: Record<string, unknown>): string | null {
  // Try IGDB cover first.
  const metas = body.metadata as Array<Record<string, unknown>> | undefined;
  if (!metas) return null;
  for (const m of metas) {
    if (m.source === 'IGDB' && typeof m.link === 'string') {
      // IGDB doesn't put a cover URL directly here; we'd need a second
      // call to MetadataProxy. Skip for now — keeping this stub so
      // future caller can wire it up without a second API roundtrip.
      return null;
    }
  }
  return null;
}
