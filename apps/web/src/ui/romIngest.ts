// ROM ingest — pull recognized ROM files out of dropped/picked files (incl.
// .zip), detect each one's system, and store them in IndexedDB. Accepts every
// retro console in systems.ts; only GBA boots today (the rest show as "coming
// soon" in the launcher).
import { unzipSync } from 'fflate';
import { nds_title, nds_icon_rgba } from '@emulators/gba';
import { addRom, addRomHandle } from './romStore';
import { detectSystem } from './systems';

const ICON_BYTES = 32 * 32 * 4;
// Don't read an entire file just to parse its banner — only NDS ROMs this small
// (≤ 512 MB) get fully read for icon/title decode; bigger media stays on disk.
const NDS_PARSE_LIMIT = 512 * 1024 * 1024;

const isRom = (name: string) => detectSystem(name) !== null;

/** Pull every recognized ROM member out of a ZIP archive. */
export function extractRomsFromZip(zipBytes: Uint8Array): Array<{ filename: string; bytes: Uint8Array }> {
  const out: Array<{ filename: string; bytes: Uint8Array }> = [];
  const entries = unzipSync(zipBytes, { filter: (file) => isRom(file.name) });
  for (const [path, bytes] of Object.entries(entries)) {
    out.push({ filename: path.split('/').pop() || path, bytes });
  }
  return out;
}

export interface IngestResult {
  added: number;
  failed: number;
  messages: string[];
}

/** Store every recognized ROM found in the given files (zip members included). */
export async function ingestFiles(files: FileList | File[]): Promise<IngestResult> {
  const messages: string[] = [];
  const queue: Array<{ filename: string; bytes: Uint8Array }> = [];

  for (const file of Array.from(files)) {
    const lower = file.name.toLowerCase();
    const raw = new Uint8Array(await file.arrayBuffer());
    if (lower.endsWith('.zip')) {
      try {
        const extracted = extractRomsFromZip(raw);
        if (extracted.length === 0) { messages.push(`${file.name}: no ROMs inside`); continue; }
        queue.push(...extracted);
      } catch (e) {
        messages.push(`${file.name}: zip read failed — ${(e as Error).message}`);
      }
    } else if (isRom(file.name)) {
      queue.push({ filename: file.name, bytes: raw });
    } else {
      messages.push(`${file.name}: unrecognized ROM type`);
    }
  }

  let added = 0;
  let failed = 0;
  for (const { filename, bytes } of queue) {
    try {
      const system = detectSystem(filename) ?? 'gba';
      const opts: { title?: string; icon?: Uint8Array } = {};
      // NDS ROMs embed a banner (icon + title); decode it in the Rust core.
      if (system === 'nds') {
        try {
          const t = nds_title(bytes);
          if (t) opts.title = t;
          const ic = nds_icon_rgba(bytes);
          if (ic.length === ICON_BYTES) opts.icon = ic;
        } catch { /* wasm not ready or unparseable banner — fall back to filename */ }
      }
      await addRom(filename, bytes, system, opts);
      messages.push(`added ${filename}`);
      added++;
    } catch (e) {
      messages.push(`add ${filename} failed: ${(e as Error).message}`);
      failed++;
    }
  }
  return { added, failed, messages };
}

/** Store ROMs from on-disk file handles (File System Access API) — the file
 * stays on disk; only the handle + metadata are saved. Header fields are parsed
 * from a small slice so a multi-GB disc is never read into memory just to add
 * it. A picked `.zip` falls back to the byte path (read + extract its members).
 */
export async function ingestHandles(handles: FileSystemFileHandle[]): Promise<IngestResult> {
  const messages: string[] = [];
  let added = 0;
  let failed = 0;

  for (const handle of handles) {
    const filename = handle.name;
    try {
      // Zip archives must be read + unzipped — delegate to the byte path.
      if (filename.toLowerCase().endsWith('.zip')) {
        const file = await handle.getFile();
        const res = await ingestFiles([file]);
        added += res.added;
        failed += res.failed;
        messages.push(...res.messages);
        continue;
      }
      if (!isRom(filename)) {
        messages.push(`${filename}: unrecognized ROM type`);
        failed++;
        continue;
      }
      const file = await handle.getFile();
      const system = detectSystem(filename) ?? 'gba';
      const opts: { title?: string; code?: string; icon?: Uint8Array; size?: number } = {
        size: file.size,
      };
      if (system === 'gba') {
        // Header lives in the first 0xB0 bytes — read just that slice.
        const head = new Uint8Array(await file.slice(0, 0xB0).arrayBuffer());
        const dec = new TextDecoder('ascii');
        opts.title = dec.decode(head.subarray(0xA0, 0xAC)).replace(/\0/g, '').trim() || undefined;
        opts.code = dec.decode(head.subarray(0xAC, 0xB0));
      } else if (system === 'nds' && file.size <= NDS_PARSE_LIMIT) {
        // The NDS banner decoder indexes into the whole ROM; only small enough.
        try {
          const bytes = new Uint8Array(await file.arrayBuffer());
          const t = nds_title(bytes);
          if (t) opts.title = t;
          const ic = nds_icon_rgba(bytes);
          if (ic.length === ICON_BYTES) opts.icon = ic;
        } catch { /* wasm not ready / unparseable — fall back to filename */ }
      }
      await addRomHandle(handle, system, opts);
      messages.push(`added ${filename}`);
      added++;
    } catch (e) {
      messages.push(`add ${filename} failed: ${(e as Error).message}`);
      failed++;
    }
  }
  return { added, failed, messages };
}
