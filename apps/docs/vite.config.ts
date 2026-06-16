import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

// The docs site is a plain static SPA — no Cloudflare Worker, no API routes.
// `vite build` emits a static bundle to dist/, which wrangler (or any static
// host) serves directly with an SPA fallback for client-side routes.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    target: 'esnext',
  },
});
