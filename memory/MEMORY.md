# Memory Index

- [PS1 BIOS-less strategy](ps1-biosless-strategy.md) — hybrid: bundle OpenBIOS through the existing real-BIOS path; HLE is a later fallback. Validate against Spyro first.
- [Crash screens](crash-screens.md) — every core draws its own fault screen in Rust (not React); per-arch triggers.
- [GameCube core](gamecube-core.md) — core-gc scaffolded (Gekko CPU foundation, big-endian); foundation only, not working.
- [Agent task sizing](agent-task-sizing.md) — keep background agents under ~30 min; split open-ended work into diagnose-then-fix stages.
- [Docs app](docs-app.md) — apps/docs static SPA; per-core support data in src/cores.ts. Keep it updated when cores change.
