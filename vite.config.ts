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
    fs: { allow: ['..'] },
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'require-corp',
    },
  },
  build: {
    target: 'esnext',
  },
  assetsInclude: ['**/*.gba'],
});