/*
 * Plain (framework-free) service worker for emulators.
 *
 * Strategy:
 *  - Precache the app shell on install so the app boots offline. We can't
 *    know the hashed asset filenames at build time without a plugin, so the
 *    shell precache is intentionally small (just "/") and the rest of the
 *    static assets are cached lazily on first fetch (runtime caching).
 *  - Navigations + same-origin static assets: cache-first (with a background
 *    network update on navigations so the shell stays fresh).
 *  - Everything else: network-first, falling back to cache when offline.
 *
 * ROM/save data already live in IndexedDB/localStorage, so the bundled *.gba
 * files in public/ are deliberately NOT cached here (they are huge and would
 * blow the cache budget).
 *
 * Bump CACHE_VERSION whenever the caching behaviour changes so old caches get
 * cleaned up on activate.
 */
const CACHE_VERSION = 'v2';
const CACHE_NAME = `emulators-${CACHE_VERSION}`;

// Dev-host kill switch. The `portless` dev proxy serves the app at
// `*.localhost`, where a cache-first SW intercepts Vite's `/@vite/client`,
// `/src/*.tsx`, `/@react-refresh` module requests and replays stale/corrupt
// copies (NS_ERROR_CORRUPTED_CONTENT). On any dev host this SW refuses to
// install, passes every fetch straight through, and unregisters itself +
// purges caches — so a stale registration self-heals on the next navigation
// (the browser always update-checks /sw.js against the network).
const DEV_HOST =
  self.location.hostname === 'localhost' ||
  self.location.hostname === '127.0.0.1' ||
  self.location.hostname === '0.0.0.0' ||
  self.location.hostname.endsWith('.localhost');

// Minimal app shell. Hashed JS/CSS bundles are picked up by runtime caching.
const PRECACHE_URLS = ['/', '/index.html', '/manifest.webmanifest', '/icon-192.png', '/icon-512.png'];

self.addEventListener('install', (event) => {
  if (DEV_HOST) {
    self.skipWaiting();
    return;
  }
  event.waitUntil(
    caches
      .open(CACHE_NAME)
      .then((cache) =>
        // Use individual adds so a single 404 doesn't abort the whole install.
        Promise.allSettled(PRECACHE_URLS.map((url) => cache.add(url))),
      )
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  if (DEV_HOST) {
    // Self-destruct on dev: drop all caches, claim clients (so this no-op SW
    // controls the page instead of the old cache-first one), then unregister.
    event.waitUntil(
      caches
        .keys()
        .then((keys) => Promise.all(keys.map((k) => caches.delete(k))))
        .then(() => self.clients.claim())
        .then(() => self.registration.unregister()),
    );
    return;
  }
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => key !== CACHE_NAME).map((key) => caches.delete(key))),
      )
      .then(() => self.clients.claim()),
  );
});

// Don't cache the bundled ROMs — they're large and the real ROM/save state
// lives in IndexedDB/localStorage.
function isRom(url) {
  return url.pathname.endsWith('.gba');
}

function isStaticAsset(url) {
  return /\.(?:js|mjs|css|woff2?|ttf|otf|png|jpe?g|gif|svg|webp|ico|json|wasm)$/.test(url.pathname);
}

self.addEventListener('fetch', (event) => {
  // Dev host: never intercept — let Vite's module/HMR requests hit the network.
  if (DEV_HOST) return;

  const { request } = event;

  // Only handle GET; let the browser deal with everything else.
  if (request.method !== 'GET') return;

  const url = new URL(request.url);
  const sameOrigin = url.origin === self.location.origin;

  // Skip cross-origin (analytics, LibRetro thumbnails, etc.) and bundled ROMs.
  if (!sameOrigin || isRom(url)) return;

  // Navigations: cache-first for the app shell so we load instantly offline,
  // with a background refresh of the cached shell.
  if (request.mode === 'navigate') {
    event.respondWith(
      caches.match(request).then((cached) => {
        const network = fetch(request)
          .then((response) => {
            if (response && response.ok) {
              const copy = response.clone();
              caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
            }
            return response;
          })
          .catch(() => cached || caches.match('/'));
        return cached || network;
      }),
    );
    return;
  }

  // Static assets (hashed bundles, fonts, icons): cache-first.
  if (isStaticAsset(url)) {
    event.respondWith(
      caches.match(request).then(
        (cached) =>
          cached ||
          fetch(request).then((response) => {
            if (response && response.ok) {
              const copy = response.clone();
              caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
            }
            return response;
          }),
      ),
    );
    return;
  }

  // Everything else: network-first, fall back to cache when offline.
  event.respondWith(
    fetch(request)
      .then((response) => {
        if (response && response.ok) {
          const copy = response.clone();
          caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
        }
        return response;
      })
      .catch(() => caches.match(request)),
  );
});
