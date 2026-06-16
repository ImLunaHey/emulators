// IndexedDB-backed ROM library. A row holds either the ROM bytes (the legacy /
// fallback path, for browsers without the File System Access API) OR an on-disk
// `FileSystemFileHandle` (preferred) — in the latter case only the handle +
// metadata live in IndexedDB and the bytes are read straight from disk at launch
// time, so huge media (a 4.7 GB Xbox disc) never gets copied into browser
// storage. The user supplies their own ROMs; nothing is shipped from the server.
import { ensureReadPermission } from './fsaccess';

const DB_NAME = 'emulators-roms';
const STORE = 'roms';
const META_KEY = 'emulators:selectedRom';

export interface RomMeta {
  id: string;             // unique slug derived from filename
  filename: string;
  title: string;          // GBA: ASCII header title; others: filename
  code: string;           // GBA: 4-char header code; others: ''
  system: string;         // SystemId ('gba'|'nds'|…) — see systems.ts
  size: number;
  addedAt: number;
  // MD5 of the ROM bytes (32 hex chars). Optional because older library
  // entries may have been added before we started hashing on import.
  md5?: string;
  // Decoded 32×32 RGBA icon (NDS banner). Absent for systems without an
  // embedded icon (e.g. GBA).
  icon?: Uint8Array;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: 'id' });
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

function slugify(name: string): string {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || `rom-${Date.now()}`;
}

export async function listRoms(): Promise<RomMeta[]> {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, 'readonly');
    const store = tx.objectStore(STORE);
    const req = store.getAll();
    req.onsuccess = () => {
      // Return only the metadata fields (strip the bytes blob).
      const out: RomMeta[] = (req.result as any[])
        .map(({ id, filename, title, code, size, addedAt, md5, system, icon }) =>
          ({ id, filename, title, code, size, addedAt, md5, system: system ?? 'gba', icon }))
        .sort((a, b) => b.addedAt - a.addedAt);
      resolve(out);
    };
    req.onerror = () => reject(req.error);
  });
}

/** Read a stored row by id (full record, including any bytes/handle). */
function getRow(id: string): Promise<any | undefined> {
  return openDb().then(
    (db) =>
      new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, 'readonly');
        const req = tx.objectStore(STORE).get(id);
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error);
      }),
  );
}

/** Get a ROM's bytes. For a disk-handle row this reads the file from disk on
 * demand (re-requesting read permission if needed — call from a user gesture);
 * for a legacy bytes row it returns the stored blob. */
export async function getRomBytes(id: string): Promise<Uint8Array | null> {
  const row = await getRow(id);
  if (!row) return null;
  const handle = row.handle as FileSystemFileHandle | undefined;
  if (handle) {
    if (!(await ensureReadPermission(handle))) {
      throw new Error(`Permission to read "${row.filename ?? id}" from disk was denied`);
    }
    const file = await handle.getFile();
    return new Uint8Array(await file.arrayBuffer());
  }
  return (row.bytes as Uint8Array | undefined) ?? null;
}

export async function addRom(
  filename: string,
  bytes: Uint8Array,
  system = 'gba',
  opts: { title?: string; icon?: Uint8Array; md5?: string } = {},
): Promise<RomMeta> {
  const base = filename.replace(/\.[^.]+$/, '');
  let title = opts.title ?? '';
  let code = '';
  // GBA's header layout is known here; other systems' titles are parsed by the
  // caller (e.g. the NDS banner) and passed in via opts.title.
  if (system === 'gba') {
    const dec = new TextDecoder('ascii');
    const t = dec.decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '').trim();
    code = dec.decode(bytes.subarray(0xAC, 0xB0));
    if (!title) title = t;
  }
  title = title || base;
  const id = slugify(base);
  const row = {
    id, filename, title, code, system,
    size: bytes.length,
    addedAt: Date.now(),
    icon: opts.icon,
    md5: opts.md5,
    bytes,
  };
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).put(row);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
  return { id, filename, title, code, system, size: bytes.length, addedAt: row.addedAt, icon: opts.icon, md5: opts.md5 };
}

// Add a ROM by on-disk handle (File System Access API). Stores only the handle
// + metadata — NOT the bytes — so the file stays on disk and is read at launch.
// The caller parses any header fields (GBA title/code, NDS banner) from a slice
// and passes them via opts, since we deliberately don't read the whole file here.
export async function addRomHandle(
  handle: FileSystemFileHandle,
  system: string,
  opts: { title?: string; code?: string; icon?: Uint8Array; md5?: string; size?: number } = {},
): Promise<RomMeta> {
  const filename = handle.name;
  const base = filename.replace(/\.[^.]+$/, '');
  const title = opts.title || base;
  const code = opts.code ?? '';
  const id = slugify(base);
  const size = opts.size ?? 0;
  const addedAt = Date.now();
  const row = { id, filename, title, code, system, size, addedAt, icon: opts.icon, md5: opts.md5, handle };
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).put(row);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
  return { id, filename, title, code, system, size, addedAt, icon: opts.icon, md5: opts.md5 };
}

// Backfill an existing ROM's md5 (older library entries pre-date the
// hash-on-import change). Keeps the bytes blob in place; just merges
// the md5 field into the existing row.
export async function updateRomMd5(id: string, md5: string): Promise<void> {
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    const store = tx.objectStore(STORE);
    const get = store.get(id);
    get.onsuccess = () => {
      const row = get.result;
      if (!row) { resolve(); return; }
      row.md5 = md5;
      const put = store.put(row);
      put.onsuccess = () => resolve();
      put.onerror = () => reject(put.error);
    };
    get.onerror = () => reject(get.error);
  });
}

export async function deleteRom(id: string): Promise<void> {
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).delete(id);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}

export function getSelectedRom(): string | null {
  return localStorage.getItem(META_KEY);
}
export function setSelectedRom(id: string | null): void {
  if (id) localStorage.setItem(META_KEY, id);
  else localStorage.removeItem(META_KEY);
}
