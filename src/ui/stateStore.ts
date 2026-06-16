// IndexedDB-backed savestate slots. Savestate blobs are ~400 KB+
// (EWRAM alone is 256 KB), well past localStorage's quota, so they live
// in their own IndexedDB database keyed by `${gameCode}#${slot}`. Each
// record also carries a small thumbnail dataURL + timestamp for the
// slot grid UI. Kept in a separate DB from the ROM library so neither
// has to version-bump the other.

const DB_NAME = 'emulators-states';
const STORE = 'states';

export interface StateMeta {
  slot: number;
  savedAt: number;
  size: number;
  thumb: string;   // data URL (small PNG of the framebuffer at save time)
}

interface StateRow extends StateMeta {
  key: string;     // `${code}#${slot}`
  code: string;
  blob: Uint8Array;
}

function openDb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB_NAME, 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) {
        const store = db.createObjectStore(STORE, { keyPath: 'key' });
        store.createIndex('code', 'code', { unique: false });
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

const keyOf = (code: string, slot: number) => `${code}#${slot}`;

export async function putState(
  code: string, slot: number, blob: Uint8Array, thumb: string,
): Promise<StateMeta> {
  const row: StateRow = {
    key: keyOf(code, slot), code, slot,
    savedAt: Date.now(), size: blob.length, thumb,
    // Copy into a plain ArrayBuffer-backed view so structured clone
    // accepts it regardless of the source buffer kind.
    blob: blob.slice(),
  };
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).put(row);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
  return { slot, savedAt: row.savedAt, size: row.size, thumb };
}

export async function getStateBlob(code: string, slot: number): Promise<Uint8Array | null> {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, 'readonly');
    const req = tx.objectStore(STORE).get(keyOf(code, slot));
    req.onsuccess = () => resolve((req.result as StateRow | undefined)?.blob ?? null);
    req.onerror = () => reject(req.error);
  });
}

// All slots for a game, without the heavy blob — just enough for the grid.
export async function listStates(code: string): Promise<StateMeta[]> {
  const db = await openDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, 'readonly');
    const req = tx.objectStore(STORE).index('code').getAll(IDBKeyRange.only(code));
    req.onsuccess = () => {
      const rows = req.result as StateRow[];
      resolve(rows.map(({ slot, savedAt, size, thumb }) => ({ slot, savedAt, size, thumb }))
        .sort((a, b) => a.slot - b.slot));
    };
    req.onerror = () => reject(req.error);
  });
}

export async function deleteState(code: string, slot: number): Promise<void> {
  const db = await openDb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).delete(keyOf(code, slot));
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}
