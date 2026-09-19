// Shell assets are public. API responses and saved pages belong to one verified
// server/account scope. Legacy yomu-v* caches are retained but never auto-served.
const SHELL_CACHE = 'yomu-shell-v8';
const CONTROL = 'yomu-control-v1';
const SHELL = '/';
const RUNTIME_LIMIT = 128 * 1024 * 1024;
let current;
const clients = new Map();
let writes = Promise.resolve();
const serial = fn => {
  const run = async () => {
    // A previous worker can finish an in-flight fetch during an upgrade.
    // Re-read the persisted fence inside the cross-worker write lock.
    current = await metaGet('owner') || { scope: null, base: null, generation: 0 };
    return fn();
  };
  const result = writes.then(() => self.navigator.locks
    ? self.navigator.locks.request('yomu-offline-storage', run) : run());
  writes = result.catch(() => {});
  return result;
};
const assetUrls = html => [...new Set([...html.matchAll(/(?:href|src|from)\s*=?\s*["']([^"']+\.(?:js|css|wasm))["']/g)]
  .map(m => new URL(m[1], self.location.origin).pathname))];
async function refreshShell(cache, response) {
  const wanted = assetUrls(await response.clone().text());
  if (!wanted.some(url => url.endsWith('.wasm'))) throw new Error('Not a Yomu shell');
  for (const url of wanted) {
    // Fixed-name glue is revalidated too, not kept forever as if fingerprinted.
    const asset = await fetch(url, { cache: 'no-cache' });
    if (!asset.ok || asset.headers.get('content-type')?.includes('text/html')) throw new Error(`Invalid boot asset ${url}`);
    await cache.put(url, asset);
  }
  await cache.put(SHELL, response);
  // Do not prune earlier assets here: another tab can still run that shell.
}
self.addEventListener('install', event => event.waitUntil((async () => {
  const response = await fetch(SHELL, { cache: 'no-cache' });
  if (!response.ok) throw new Error('Shell unavailable');
  await refreshShell(await caches.open(SHELL_CACHE), response);
  await self.skipWaiting();
})()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));
const metaUrl = key => new URL(`/.yomu/${key}`, self.location.origin).href;
async function metaGet(key) {
  const response = await (await caches.open(CONTROL)).match(metaUrl(key));
  return response ? response.json() : null;
}
async function metaPut(key, value) {
  await (await caches.open(CONTROL)).put(metaUrl(key), new Response(JSON.stringify(value)));
}
async function state() { return current ??= (await metaGet('owner') || { scope: null, base: null, generation: 0 }); }
const runtimeName = scope => `yomu-private:${scope}`;
const savedPrefix = scope => `yomu-saved:${scope}:`;
const pointer = (scope, chapter) => `saved/${encodeURIComponent(scope)}/${chapter}`;
const pending = (scope, chapter) => `pending/${encodeURIComponent(scope)}/${chapter}`;
const pagePath = (base, chapter, page) => new URL(`api/v1/units/${chapter}/pages/${page}`, base).href;
function assertScope(expected) {
  if (!expected.scope || current.scope !== expected.scope || current.generation !== expected.generation) throw new Error('Offline account changed; operation cancelled');
}
async function setScope(message, clientId) {
  const old = await state();
  if (message.scope !== old.scope || message.base !== old.base) {
    // A stale tab must not switch the worker back to its former account.
    // Offline operation can resume the existing owner, never invent a new one.
    if (message.scope || (old.scope && message.base === old.base)) {
      const response = await fetch(new URL('api/v1/auth/me', message.base), {
        cache: 'no-store', signal: AbortSignal.timeout(4000),
        headers: message.authToken ? { Authorization: `Bearer ${message.authToken}` } : {},
      });
      if (!response.ok) throw new Error('Cannot verify offline account');
      const me = await response.json();
      const verified = me.user ? `${encodeURIComponent(message.base)}/${me.user.id}` : null;
      if (verified !== message.scope) throw new Error('Account changed; reload before accessing offline data');
    }
    current = { scope: message.scope, base: message.base, generation: old.generation + 1 };
    await metaPut('owner', current);
    if (old.scope) {
      for (const name of await caches.keys()) if (name === runtimeName(old.scope) || name.startsWith(savedPrefix(old.scope))) await caches.delete(name);
      const meta = await caches.open(CONTROL);
      for (const request of await meta.keys()) if (request.url.includes(`/${encodeURIComponent(old.scope)}/`)) await meta.delete(request);
    }
  }
  clients.set(clientId, message.scope);
  return true;
}
async function savedCheck(scope, chapter, pages) {
  const manifest = await metaGet(pointer(scope, chapter));
  if (!manifest || manifest.pages !== pages || !Number.isInteger(pages) || pages <= 0) return false;
  if (!(await caches.has(manifest.cache))) return false;
  const cache = await caches.open(manifest.cache);
  for (let n = 0; n < pages; n++) if (!(await cache.match(pagePath(manifest.base, chapter, n)))) return false;
  return true;
}
async function savedImage(expected, url) {
  const match = url.pathname.match(/\/api\/v1\/units\/([^/]+)\/pages\/(\d+)$/);
  if (!match) return null;
  const manifest = await metaGet(pointer(expected.scope, match[1]));
  if (!manifest || !(await caches.has(manifest.cache))) return null;
  return (await caches.open(manifest.cache)).match(pagePath(manifest.base, match[1], Number(match[2])));
}
async function storedResponse(response) {
  const bytes = await response.arrayBuffer();
  const headers = new Headers(response.headers);
  headers.delete('content-encoding'); headers.delete('content-length');
  headers.set('x-yomu-stored-bytes', String(bytes.byteLength));
  return new Response(bytes, { status: response.status, headers });
}
async function runtimePut(expected, request, response) {
  const stored = await storedResponse(response);
  await serial(async () => {
    assertScope(expected);
    const cache = await caches.open(runtimeName(expected.scope));
    try { await cache.put(request, stored); }
    catch (error) {
      if (error.name !== 'QuotaExceededError') throw error;
      await caches.delete(runtimeName(expected.scope)); // runtime only, never saved chapters
      return;
    }
    let bytes = 0;
    const keys = await cache.keys();
    for (let i = keys.length - 1; i >= 0; i--) {
      bytes += Number((await cache.match(keys[i])).headers.get('x-yomu-stored-bytes')) || 0;
      if (bytes > RUNTIME_LIMIT) await cache.delete(keys[i]);
    }
  });
}
async function command(message, source) {
  if (message.type === 'set-scope') return serial(() => setScope(message, source.id));
  const expected = { ...await state() };
  if (message.scope !== expected.scope || clients.get(source.id) !== expected.scope) throw new Error('Offline account is not initialized');
  assertScope(expected);
  const { chapter, page, pages } = message;
  if (chapter !== undefined && !/^[0-9a-f-]{36}$/i.test(chapter)) throw new Error('Invalid chapter');
  if (['save-page', 'save-finish', 'saved-check'].includes(message.type)) {
    const n = message.type === 'save-page' ? page : pages;
    if (!Number.isInteger(n) || n < 0 || n > 100000) throw new Error('Invalid page count');
  }
  if (message.type === 'saved-check') return savedCheck(expected.scope, chapter, pages);
  if (message.type === 'save-page') {
    const url = new URL(message.url, expected.base);
    const canonical = pagePath(expected.base, chapter, page);
    if (url.origin + url.pathname !== new URL(canonical).origin + new URL(canonical).pathname) throw new Error('Invalid saved page URL');
    const started = await metaGet(pending(expected.scope, chapter));
    if (!started) throw new Error('Device save was interrupted; retry');
    const response = await fetch(url);
    if (!response.ok) throw new Error(`Page ${page}: HTTP ${response.status}`);
    const stored = await storedResponse(response);
    return serial(async () => {
      assertScope(expected);
      const stage = await metaGet(pending(expected.scope, chapter));
      if (!stage || stage.cache !== started.cache) throw new Error('Device save was interrupted; retry');
      const cache = await caches.open(stage.cache);
      try { await cache.put(canonical, stored.clone()); }
      catch (error) {
        if (error.name !== 'QuotaExceededError') throw error;
        await caches.delete(runtimeName(expected.scope));
        try { await cache.put(canonical, stored); }
        catch { throw new Error('Storage quota exceeded; remove saved chapters and retry'); }
      }
      return true;
    });
  }
  return serial(async () => {
    assertScope(expected);
    const meta = await caches.open(CONTROL);
    if (message.type === 'save-begin') {
      const previous = await metaGet(pending(expected.scope, chapter));
      if (previous) await caches.delete(previous.cache);
      await metaPut(pending(expected.scope, chapter), { cache: `${savedPrefix(expected.scope)}${chapter}:${crypto.randomUUID()}` });
      return true;
    }
    if (message.type === 'save-cancel') {
      const stage = await metaGet(pending(expected.scope, chapter));
      if (stage) await caches.delete(stage.cache);
      await meta.delete(metaUrl(pending(expected.scope, chapter)));
      return true;
    }
    if (message.type === 'save-finish') {
      const stage = await metaGet(pending(expected.scope, chapter));
      if (!stage || !pages || !(await caches.has(stage.cache))) throw new Error('Incomplete device save');
      const cache = await caches.open(stage.cache);
      let bytes = 0;
      for (let n = 0; n < pages; n++) {
        const response = await cache.match(pagePath(expected.base, chapter, n));
        if (!response) throw new Error(`Device save is missing page ${n}`);
        bytes += Number(response.headers.get('x-yomu-stored-bytes')) || 0;
      }
      const old = await metaGet(pointer(expected.scope, chapter));
      // Publishing the manifest is the commit point. A failed replacement
      // never deletes the previously complete copy.
      await metaPut(pointer(expected.scope, chapter), { ...stage, pages, bytes, base: expected.base });
      await meta.delete(metaUrl(pending(expected.scope, chapter)));
      if (old && old.cache !== stage.cache) await caches.delete(old.cache);
      return true;
    }
    if (message.type === 'saved-delete') {
      const old = await metaGet(pointer(expected.scope, chapter));
      await meta.delete(metaUrl(pointer(expected.scope, chapter)));
      if (old) await caches.delete(old.cache);
      const runtime = await caches.open(runtimeName(expected.scope));
      for (const request of await runtime.keys()) if (new URL(request.url).pathname.includes(`/units/${chapter}/`)) await runtime.delete(request);
      return true;
    }
    if (message.type === 'storage-report') {
      let bytes = 0;
      for (const request of await meta.keys()) if (request.url.includes(`/saved/${encodeURIComponent(expected.scope)}/`)) bytes += (await (await meta.match(request)).json()).bytes || 0;
      return bytes;
    }
    if (message.type === 'runtime-clear') { await caches.delete(runtimeName(expected.scope)); return true; }
    if (message.type === 'adopt-legacy') {
      // Explicit owner consent is required, including in shared-account mode,
      // by the UI. Originals stay untouched for recovery.
      const cache = await caches.open(runtimeName(expected.scope));
      for (const name of await caches.keys()) if (/^yomu-v\d+$/.test(name)) {
        const legacy = await caches.open(name);
        for (const request of await legacy.keys()) {
          const path = new URL(request.url).pathname;
          if (path.startsWith('/api/v1/') && !path.startsWith('/api/v1/auth/')) await cache.put(request, await legacy.match(request));
        }
      }
      for (const [chapter, mark] of Object.entries(message.marks || {})) {
        const pages = typeof mark === 'number' ? mark : mark.pages;
        if (!/^[0-9a-f-]{36}$/i.test(chapter) || !Number.isInteger(pages) || pages <= 0 || pages > 100000) continue;
        if (await savedCheck(expected.scope, chapter, pages)) continue;
        const responses = [];
        for (let n = 0; n < pages; n++) {
          const response = await cache.match(pagePath(expected.base, chapter, n), { ignoreVary: true });
          if (!response) break;
          responses.push(response);
        }
        if (responses.length !== pages) continue;
        const name = `${savedPrefix(expected.scope)}${chapter}:${crypto.randomUUID()}`;
        const saved = await caches.open(name);
        let bytes = 0;
        for (let n = 0; n < pages; n++) {
          const response = await storedResponse(responses[n]);
          bytes += Number(response.headers.get('x-yomu-stored-bytes'));
          await saved.put(pagePath(expected.base, chapter, n), response);
        }
        await metaPut(pointer(expected.scope, chapter), { cache: name, pages, bytes, base: expected.base });
      }
      return true;
    }
    throw new Error('Unknown offline storage operation');
  });
}
self.addEventListener('message', event => {
  if (!event.source || !event.ports[0]) return;
  event.waitUntil(command(event.data, event.source).then(
    value => event.ports[0].postMessage({ value }),
    error => event.ports[0].postMessage({ error: error.message || String(error) }),
  ));
});
async function privateRead(event, url) {
  const expected = { ...await state() };
  if (!expected.scope || clients.get(event.clientId) !== expected.scope) return new Response('Offline account is not initialized; reload', { status: 401 });
  const image = /\/units\/[^/]+\/pages\/\d+$|\/publications\/[^/]+\/cover$/.test(url.pathname);
  const cache = await caches.open(runtimeName(expected.scope));
  if (image) {
    const saved = await savedImage(expected, url);
    const cached = saved || await cache.match(event.request);
    assertScope(expected);
    if (cached) return cached;
  }
  try {
    const response = await fetch(event.request);
    assertScope(expected);
    if (response.ok) {
      // Runtime persistence is best effort. Explicit saves use acknowledged RPCs.
      event.waitUntil(runtimePut(expected, event.request, response.clone()).catch(() => {}));
    }
    return response;
  } catch (error) {
    assertScope(expected);
    const cached = await cache.match(event.request);
    if (cached) return cached;
    throw error;
  }
}
self.addEventListener('fetch', event => {
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin) return;
  if (url.pathname.startsWith('/api/')) {
    // Identity and reachability are NEVER answered from a cached account.
    if (url.pathname.startsWith('/api/v1/auth/') || /\/api\/v1\/(health|metrics)/.test(url.pathname)) return;
    if (event.request.method !== 'GET') {
      event.respondWith((async () => {
        const active = await state();
        if (!active.scope || clients.get(event.clientId) !== active.scope) return new Response('Account changed; reload before modifying data', { status: 409 });
        return fetch(event.request);
      })());
    } else if (event.request.mode !== 'navigate') event.respondWith(privateRead(event, url));
    return;
  }
  if (event.request.method !== 'GET') return;
  event.respondWith((async () => {
    const cache = await caches.open(SHELL_CACHE);
    if (event.request.mode === 'navigate') {
      try {
        const response = await fetch(SHELL);
        if (response.ok) { event.waitUntil(serial(() => refreshShell(cache, response.clone())).catch(() => {})); return response; }
        return await cache.match(SHELL) || response;
      } catch (error) { const shell = await cache.match(SHELL); if (shell) return shell; throw error; }
    }
    // Public shell bytes do not vary by identity/origin. CORS module/preload
    // requests carry different Origin headers than the worker's precache fetch;
    // honoring the server's CORS Vary here would miss a perfectly cached asset.
    const cached = await cache.match(url.pathname, { ignoreVary: true });
    return cached || fetch(event.request);
  })());
});
