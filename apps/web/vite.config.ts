import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { cloudflare } from "@cloudflare/vite-plugin";

// The Cloudflare plugin spins up worker environments that aren't
// compatible with vitest's Node externals, so we only enable it for
// the actual dev/build pipeline. Vitest sets process.env.VITEST.
const isTest = !!process.env.VITEST;

export default defineConfig({
  plugins: [react(), tailwindcss(), ...(isTest ? [] : [cloudflare()])],
  server: {
    // Allow serving files from the monorepo root (apps/web -> ../..), so the
    // workspace-linked core packages under packages/*/pkg can be read by Vite.
    fs: { allow: ['../..'] },
    // No COOP / COEP — we don't use SharedArrayBuffer or atomics, and
    // `require-corp` blocks cross-origin images (LibRetro thumbnails)
    // since their server doesn't send Cross-Origin-Resource-Policy.
  },
  build: {
    target: 'esnext',
  },
  assetsInclude: ['**/*.gba'],
});