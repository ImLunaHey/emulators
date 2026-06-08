// IndexedDB-backed ROM library. Individual ROM files are 8-32 MB each
// (far larger than localStorage's ~5 MB quota), so we shelve them in
// IndexedDB and remember the user's selection. The user uploads their
// own .gba files; nothing ROM-related is ever shipped from the server.
const DB_NAME = 'gba-recomp-roms';
const STORE = 'roms';
const META_KEY = 'gba-recomp:selectedRom';
function openDb() {
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
function slugify(name) {
    return name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || `rom-${Date.now()}`;
}
export async function listRoms() {
    const db = await openDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, 'readonly');
        const store = tx.objectStore(STORE);
        const req = store.getAll();
        req.onsuccess = () => {
            // Return only the metadata fields (strip the bytes blob).
            const out = req.result
                .map(({ id, filename, title, code, size, addedAt }) => ({ id, filename, title, code, size, addedAt }))
                .sort((a, b) => b.addedAt - a.addedAt);
            resolve(out);
        };
        req.onerror = () => reject(req.error);
    });
}
export async function getRomBytes(id) {
    const db = await openDb();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, 'readonly');
        const store = tx.objectStore(STORE);
        const req = store.get(id);
        req.onsuccess = () => {
            const row = req.result;
            resolve(row?.bytes ?? null);
        };
        req.onerror = () => reject(req.error);
    });
}
export async function addRom(filename, bytes) {
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
        bytes,
    };
    const db = await openDb();
    await new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, 'readwrite');
        tx.objectStore(STORE).put(row);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
    });
    return { id: row.id, filename, title: row.title, code, size: bytes.length, addedAt: row.addedAt };
}
export async function deleteRom(id) {
    const db = await openDb();
    await new Promise((resolve, reject) => {
        const tx = db.transaction(STORE, 'readwrite');
        tx.objectStore(STORE).delete(id);
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
    });
}
export function getSelectedRom() {
    return localStorage.getItem(META_KEY);
}
export function setSelectedRom(id) {
    if (id)
        localStorage.setItem(META_KEY, id);
    else
        localStorage.removeItem(META_KEY);
}
