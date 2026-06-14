// ROM ingest — pull recognized ROM files out of dropped/picked files (incl.
// .zip), detect each one's system, and store them in IndexedDB. Accepts every
// retro console in systems.ts; only GBA boots today (the rest show as "coming
// soon" in the launcher).
import { unzipSync } from 'fflate';
import { nds_title, nds_icon_rgba } from '../../core/pkg/gba_core.js';
import { addRom } from './romStore';
import { detectSystem } from './systems';

const ICON_BYTES = 32 * 32 * 4;

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
