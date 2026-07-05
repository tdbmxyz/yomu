// yomu service worker: makes the app and cached content work offline.
//
// Strategy:
// - page images & covers: cache-first (immutable content, saved on first
//   fetch — "save to device" simply fetches every page once)
// - other GET /api/v1/*: network-first with cache fallback, so library and
//   chapter lists stay browsable offline (possibly stale)
// - app shell (/, hashed .js/.wasm/.css): cache-first with background
//   refresh of '/'; hashed assets are immutable by construction
//
// Bump CACHE when the caching logic changes.
const CACHE = "yomu-v1";

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(["/"]))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))),
      )
      .then(() => self.clients.claim()),
  );
});

const isImage = (url) =>
  /\/api\/v1\/chapters\/[^/]+\/pages\/\d+$/.test(url.pathname) ||
  /\/api\/v1\/manga\/[^/]+\/cover$/.test(url.pathname);

async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) return cached;
  const response = await fetch(request);
  if (response.ok) {
    const cache = await caches.open(CACHE);
    cache.put(request, response.clone());
  }
  return response;
}

async function networkFirst(request, fallbackUrl) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      const cache = await caches.open(CACHE);
      cache.put(request, response.clone());
    }
    return response;
  } catch (err) {
    const cached = await caches.match(fallbackUrl ?? request);
    if (cached) return cached;
    throw err;
  }
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  if (request.mode === "navigate") {
    // SPA: any route is served by the shell.
    event.respondWith(networkFirst(request, "/"));
  } else if (isImage(url)) {
    event.respondWith(cacheFirst(request));
  } else if (url.pathname.startsWith("/api/")) {
    event.respondWith(networkFirst(request));
  } else {
    // Hashed static assets.
    event.respondWith(cacheFirst(request));
  }
});
