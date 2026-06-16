# Docs app

`apps/docs` is a static React + Vite + Tailwind SPA (no Cloudflare Worker — pure
assets deploy via wrangler, SPA fallback) documenting per-core support. All
content lives in one typed registry: `apps/docs/src/cores.ts` (`CORES: Core[]`),
with per-core CPU/video/audio/saves/input prose, file formats, implemented
features, known gaps, test-suite summary, and a tested-games table. Each UI
component is its own file under `apps/docs/src/ui/`.

Each core also has a fine-grained **capability matrix** in
`apps/docs/src/matrices.ts` (`MATRICES: Record<id, MatrixGroup[]>`, rendered by
`SupportMatrix`): per-subsystem rows scored yes / partial / testing / no,
benchmarked against the emulation-general wiki's per-system feature/peripheral
comparisons (and Shonumi's "State of Emulation 2024" for the GB family).

**Keep both updated.** When a core gains/loses a feature, changes maturity,
passes a new game, or its test count changes, update that core's entry in
`cores.ts` AND its matrix in `matrices.ts` in the same change. The docs are hand-maintained source-of-truth prose, not
auto-generated from the Rust — so they silently go stale unless updated
alongside core work. New core → add a `Core` entry (and an accent color matching
the launcher's `SYSTEM_PRESENTATION` in `apps/web/src/ui/systems.ts`).

Related: [[crash-screens]], [[gamecube-core]], [[ps1-biosless-strategy]].
