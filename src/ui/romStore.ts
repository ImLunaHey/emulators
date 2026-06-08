// IndexedDB-backed ROM library. Individual ROM files are 8-32 MB each
// (far larger than localStorage's ~5 MB quota), so we shelve them in
// IndexedDB and remember the user's selection. The user uploads their
// own .gba files; nothing ROM-related is ever shipped from the server.

const DB_NAME = 'gba-recomp-roms';
const STORE = 'roms';
const META_KEY = 'gba-recomp:selectedRom';

export interface RomMeta {
  id: string;             // unique slug derived from filename
  filename: string;
  title: string;          // ASCII title from header 0xA0..0xAC
  code: string;           // 4-char game code from header 0xAC..0xB0
  size: number;
  addedAt: number;
  // MD5 of the ROM bytes (32 hex chars). Optional because older library
  // entries may have been added before we started hashing on import.
  md5?: string;
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
        .map(({ id, filename, title, code, size, addedAt, md5 }) => ({ id, filename, title, code, size, addedAt, md5 }))
        .sort((a, b) => b.addedAt - a.addedAt);
      resolve(out);
    };
    req.onerror = () => reject(req.error);
  });
}

export async function getRomBytes(id: string): Promise<Uint8Array | null> {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, 'readonly');
    const store = tx.objectStore(STORE);
    const req = store.get(id);
    req.onsuccess = () => {
      const row = req.result as { bytes?: Uint8Array } | undefined;
      resolve(row?.bytes ?? null);
    };
    req.onerror = () => reject(req.error);
  });
}

export async function addRom(filename: string, bytes: Uint8Array, md5?: string): Promise<RomMeta> {
  const dec = new TextDecoder('ascii');
  const title = dec.decode(bytes.subarray(0xA0, 0xAC)).replace(/\0/g, '');
  const code = dec.decode(bytes.subarray(0xAC, 0xB0));
  const id = slugify(filename.replace(/\.gba$/i, '')) || code.toLowerCase();
  const row = {
    id, filename,
    title: title.trim() || filename.replace(/\.gba$/i, ''),
    code,
    size: bytes.length,
    addedAt: Date.now(),
    md5,
    bytes,
  };
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).put(row);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
  return { id: row.id, filename, title: row.title, code, size: bytes.length, addedAt: row.addedAt, md5 };
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
