// Caches the app shell only. Anything under /api always goes to the network.
const CACHE = 'agent-desktops-shell-v3';
const SHELL = ['./', 'index.html', 'app.css', 'sky.css', 'sky.js', 'sky/deep-field.webp', 'sky/nebula-tarantula.webp', 'sky/near-stars.webp', 'sky/hires/deep-field.webp', 'sky/hires/nebula-tarantula.webp', 'app.js', 'tile.js', 'api.js', 'mock.js', 'manifest.webmanifest', 'icons/icon.svg', 'icons/icon-192.png', 'icons/icon-512.png'];

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(caches.keys().then((ks) => Promise.all(ks.filter((k) => k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim()));
});

self.addEventListener('fetch', (e) => {
  const u = new URL(e.request.url);
  if (e.request.method !== 'GET' || u.origin !== location.origin || u.pathname.startsWith('/api/')) return;
  // Network first so a rebuilt shell shows up on the next load, cache when offline.
  e.respondWith(
    fetch(e.request).then((res) => {
      if (res.ok && SHELL.some((p) => u.pathname.endsWith(p.replace('./', '')) || u.pathname === '/')) {
        const copy = res.clone();
        caches.open(CACHE).then((c) => c.put(e.request, copy));
      }
      return res;
    }).catch(() => caches.match(e.request, { ignoreSearch: true })),
  );
});
