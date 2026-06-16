import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './ui/App';
import './index.css';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

// Register the service worker for PWA / offline support. We skip it on the
// Vite dev server so HMR isn't disrupted, and only run when the browser
// actually supports service workers. (We detect dev via hostname rather than
// import.meta.env to avoid pulling in vite/client types, since this project's
// tsconfig uses an explicit `types` allowlist.)
//
// The dev host includes `*.localhost` — the `portless` proxy serves the app at
// e.g. https://emulators.localhost. A cache-first SW there intercepts Vite's
// `/@vite/client`, `/src/*.tsx`, `/@react-refresh` module requests and replays
// stale/corrupt copies (NS_ERROR_CORRUPTED_CONTENT), so we must NOT register on
// dev — and we proactively unregister any stale SW + drop its caches so a
// previously-registered one stops breaking the dev server.
const host = window.location.hostname;
const isDevHost =
  host === 'localhost' ||
  host === '127.0.0.1' ||
  host === '0.0.0.0' ||
  host.endsWith('.localhost');
if ('serviceWorker' in navigator) {
  if (isDevHost) {
    navigator.serviceWorker
      .getRegistrations()
      .then((regs) => regs.forEach((r) => r.unregister()))
      .catch(() => {});
    if (typeof caches !== 'undefined') {
      caches.keys().then((keys) => keys.forEach((k) => caches.delete(k))).catch(() => {});
    }
  } else {
    window.addEventListener('load', () => {
      navigator.serviceWorker.register('/sw.js').catch((err) => {
        console.error('Service worker registration failed:', err);
      });
    });
  }
}
