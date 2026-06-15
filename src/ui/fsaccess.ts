// File System Access API helpers (Chromium-only; feature-detected). Lets the
// launcher reference a ROM by its on-disk handle instead of copying the bytes
// into IndexedDB — essential for large media (a 4.7 GB Xbox disc would never
// fit in browser storage, and copying it wastes gigabytes). On browsers without
// the API (Firefox/Safari) callers fall back to the byte-copy path in romStore.

// `FileSystemFileHandle` is in lib.dom, but showOpenFilePicker, the permission
// methods, and getAsFileSystemHandle are not yet standardized in TS's lib —
// declare the subset we use.
type FsPermissionDescriptor = { mode?: 'read' | 'readwrite' };

declare global {
  interface FileSystemHandle {
    queryPermission?(desc?: FsPermissionDescriptor): Promise<PermissionState>;
    requestPermission?(desc?: FsPermissionDescriptor): Promise<PermissionState>;
  }
  interface DataTransferItem {
    getAsFileSystemHandle?(): Promise<FileSystemHandle | null>;
  }
  interface Window {
    showOpenFilePicker?(options?: {
      multiple?: boolean;
      excludeAcceptAllOption?: boolean;
      types?: Array<{ description?: string; accept: Record<string, string[]> }>;
    }): Promise<FileSystemFileHandle[]>;
  }
}

/** True when the browser can hand out persistent on-disk file handles. */
export function supportsFsa(): boolean {
  return typeof window !== 'undefined' && typeof window.showOpenFilePicker === 'function';
}

/** Show the OS file picker for ROM files, returning on-disk handles (empty if
 * the user cancels). `extensions` are dotted, e.g. ['.gba', '.iso']. */
export async function pickRomHandles(extensions: string[]): Promise<FileSystemFileHandle[]> {
  if (!window.showOpenFilePicker) return [];
  try {
    return await window.showOpenFilePicker({
      multiple: true,
      excludeAcceptAllOption: false,
      types: [{ description: 'Game ROMs / discs', accept: { 'application/octet-stream': extensions } }],
    });
  } catch (e) {
    // AbortError = the user dismissed the picker; treat as "nothing picked".
    if ((e as DOMException)?.name === 'AbortError') return [];
    throw e;
  }
}

/** Ensure we still hold read permission for a persisted handle. May prompt the
 * user (must be called from a user gesture, e.g. a click). */
export async function ensureReadPermission(handle: FileSystemFileHandle): Promise<boolean> {
  const opt: FsPermissionDescriptor = { mode: 'read' };
  if (!handle.queryPermission && !handle.requestPermission) return true; // older impl: assume readable
  if (handle.queryPermission && (await handle.queryPermission(opt)) === 'granted') return true;
  if (handle.requestPermission && (await handle.requestPermission(opt)) === 'granted') return true;
  return false;
}

/** Pull a file handle out of a drag-and-drop item, if the browser supports it. */
export async function handleFromDropItem(item: DataTransferItem): Promise<FileSystemFileHandle | null> {
  if (!item.getAsFileSystemHandle) return null;
  const h = await item.getAsFileSystemHandle();
  return h && h.kind === 'file' ? (h as FileSystemFileHandle) : null;
}
