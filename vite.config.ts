import { defineConfig } from 'vite';

export default defineConfig({
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
