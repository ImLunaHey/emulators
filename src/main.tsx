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
// Vite dev server (localhost / 127.0.0.1) so HMR isn't disrupted, and only
// run when the browser actually supports service workers. (We detect dev via
// hostname rather than import.meta.env to avoid pulling in vite/client types,
// since this project's tsconfig uses an explicit `types` allowlist.)
const isDevHost = ['localhost', '127.0.0.1', '0.0.0.0'].includes(window.location.hostname);
if (!isDevHost && 'serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    navigator.serviceWorker.register('/sw.js').catch((err) => {
      console.error('Service worker registration failed:', err);
    });
  });
}
