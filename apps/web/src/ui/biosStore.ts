// Tiny IndexedDB store for console BIOS ROMs (currently just PS1, which can't
// boot real discs without one). Keyed by system id.
const DB = 'emu-bios';
const STORE = 'bios';

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

export async function getBios(system: string): Promise<Uint8Array | null> {
  const db = await open();
  return new Promise((resolve, reject) => {
    const req = db.transaction(STORE, 'readonly').objectStore(STORE).get(system);
    req.onsuccess = () => resolve((req.result as Uint8Array) ?? null);
    req.onerror = () => reject(req.error);
  });
}

export async function setBios(system: string, bytes: Uint8Array): Promise<void> {
  const db = await open();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, 'readwrite');
    tx.objectStore(STORE).put(bytes, system);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
}
