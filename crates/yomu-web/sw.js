// yomu service worker: makes the app and cached content work offline.
//
// Strategy:
// - page images & covers: cache-first (immutable content, saved on first
//   fetch — "save to device" simply fetches every page once)
// - other GET /api/v1/*: network-first with cache fallback, so library and
//   chapter lists stay browsable offline (possibly stale)
// - app shell: the cache holds ONE shell entry (SHELL), refreshed by every
//   online navigation; its hashed .js/.wasm/.css are precached at install
//   and re-synced on every shell refresh, so offline boot always has the
//   exact assets the cached shell references.
//
// Bump CACHE when the caching logic changes, or when a fixed-name asset
// (favicon/app icons) changes so the old cache-first copy is purged.
const CACHE = "yomu-v5";
const SHELL = "/";

// Hashed assets referenced by a shell document (href/src/import of
// .js/.css/.wasm — what trunk emits).
const assetUrls = (html) => {
  const urls = new Set();
  for (const match of html.matchAll(/(?:href|src|from)\s*=?\s*["']([^"']+\.(?:js|css|wasm))["']/g)) {
    urls.add(new URL(match[1], self.location.origin).pathname);
  }
  return [...urls];
};

// Cache a freshly fetched shell and its hashed assets. Assets go in FIRST:
// if we go offline halfway, the cached shell must never reference assets
// that didn't make it. Stale assets of older deploys are pruned at the end.
async function refreshShell(cache, response) {
  const html = await response.clone().text();
  const wanted = assetUrls(html);
  await Promise.all(
    wanted.map(async (url) => {
      if (!(await cache.match(url))) {
        const asset = await fetch(url);
        if (!asset.ok) throw new Error(`precache ${url}: ${asset.status}`);
        await cache.put(url, asset);
      }
    }),
  );
  await cache.put(SHELL, response);
  for (const key of await cache.keys()) {
    const path = new URL(key.url).pathname;
    if (/\.(?:js|css|wasm)$/.test(path) && !wanted.includes(path)) {
      await cache.delete(key);
    }
  }
}

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(CACHE);
      const shell = await fetch(SHELL);
      // Precache shell + hashed assets: offline must work from the very
      // first session, before any navigation went through this worker.
      if (shell.ok) await refreshShell(cache, shell);
      await self.skipWaiting();
    })(),
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

async function networkFirst(request) {
  try {
    const response = await fetch(request);
    if (response.ok) {
      const cache = await caches.open(CACHE);
      cache.put(request, response.clone());
    }
    return response;
  } catch (err) {
    const cached = await caches.match(request);
    if (cached) return cached;
    throw err;
  }
}

// SPA navigations all serve the shell: cache one copy under SHELL (not per
// visited URL), refresh it — and its asset set — on every online load. The
// refresh runs after the response is served (waitUntil); if it fails
// mid-way the previous consistent shell+assets stay in place.
async function navigate(event) {
  const cache = await caches.open(CACHE);
  try {
    const response = await fetch(SHELL);
    if (response.ok) {
      event.waitUntil(refreshShell(cache, response.clone()).catch(() => {}));
      return response;
    }
    // A 5xx/redirect on "/" would otherwise be shown as the app: prefer the
    // last known-good shell when we have one.
    const cached = await cache.match(SHELL);
    return cached || response;
  } catch (err) {
    const cached = await cache.match(SHELL);
    if (cached) return cached;
    throw err;
  }
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;

  if (url.pathname.startsWith("/api/")) {
    // Before the navigate branch: /api/v1/auth/login|callback are full-page
    // navigations that must reach the server (they redirect to/from the
    // identity provider), never be answered with the app shell.
    if (request.mode === "navigate") return;
    if (isImage(url)) {
      event.respondWith(cacheFirst(request));
    } else {
      event.respondWith(networkFirst(request));
    }
  } else if (request.mode === "navigate") {
    event.respondWith(navigate(event));
  } else {
    // Hashed static assets: immutable by construction.
    event.respondWith(cacheFirst(request));
  }
});
