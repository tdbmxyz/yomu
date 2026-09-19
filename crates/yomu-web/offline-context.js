// Browser/native offline ownership. Loaded before WASM; no credentials in keys.
// Unknown legacy data is retained, never silently adopted by an OIDC account.
(() => {
  let base, scope = null, owner = null, frozen = false;
  let durable = Promise.resolve();
  const native = () => window.__TAURI__?.core?.invoke;
  const owned = key => key.startsWith('yomu-state:') || key.startsWith('yomu-owner:') || key === 'yomu-active-server' || key === 'yomu-legacy-imported';
  function persist(key, value) {
    const invoke = native();
    if (!invoke) return Promise.resolve();
    const write = () => invoke(value == null ? 'store_remove' : 'store_put', { key, value });
    const result = durable.then(write);
    // Keep serialization after failure, but expose the error to callers.
    durable = result.catch(error => { console.error('Durable offline state write failed', error); });
    return result;
  }
  async function put(key, value) {
    if (value == null) localStorage.removeItem(key); else localStorage.setItem(key, value);
    await persist(key, value);
  }
  const ownerKey = () => `yomu-owner:${encodeURIComponent(base)}`;
  const prefix = () => `yomu-state:${scope || `${encodeURIComponent(base)}/signed-out`}:`;
  const key = logical => prefix() + logical;
  async function worker(message) {
    if (!navigator.serviceWorker?.controller) throw new Error('Offline cache unavailable: no controlling Service Worker');
    return new Promise((resolve, reject) => {
      let port, settled = false;
      const finish = (error, value) => {
        if (settled) return;
        settled = true; clearTimeout(timer); port?.close();
        navigator.serviceWorker.removeEventListener('controllerchange', changed);
        error ? reject(new Error(error)) : resolve(value);
      };
      const send = () => {
        port?.close();
        const channel = new MessageChannel(); port = channel.port1;
        port.onmessage = ({ data }) => finish(data.error, data.value);
        navigator.serviceWorker.controller.postMessage({ ...message, scope, base,
          authToken: message.type === 'set-scope' ? localStorage.getItem('yomu-session') : undefined,
        }, [channel.port2]);
      };
      const changed = () => {
        // The legacy worker has no RPC handler. Retry its initialization
        // handshake on takeover, not after a full timeout. Saves must restart.
        if (message.type === 'set-scope') send();
        else finish('Offline worker changed; retry the save');
      };
      const timer = setTimeout(() => finish('Offline storage operation timed out'), 60_000);
      navigator.serviceWorker.addEventListener('controllerchange', changed);
      send();
    });
  }
  async function who() {
    const token = localStorage.getItem('yomu-session');
    const response = await fetch(new URL('api/v1/auth/me', base), {
      cache: 'no-store', headers: token ? { Authorization: `Bearer ${token}` } : {},
      signal: AbortSignal.timeout(4000),
    });
    if (!response.ok) throw new Error(`Identity verification failed: ${response.status}`);
    const me = await response.json();
    if (!['single', 'oidc'].includes(me.mode)) throw new Error('Invalid identity response');
    return me;
  }
  const identityScope = me => me?.user ? `${encodeURIComponent(base)}/${me.user.id}` : null;
  const legacyKeys = () => Object.keys(localStorage).filter(k =>
    ['yomu-outbox', 'yomu-marks-outbox', 'yomu-device-chapters', 'yomu-pull-queue', 'yomu-updates-seen'].includes(k) || k.startsWith('yomu-cache:'));
  async function importLegacy() {
    if (!scope || frozen || localStorage.getItem('yomu-legacy-imported')) return;
    if (!(await verify())) throw new Error('Verify this account online before importing retained data');
    // Merge by identity, never overwrite a newer scoped value. Keep originals.
    for (const logical of legacyKeys()) {
      const old = localStorage.getItem(logical);
      const existing = localStorage.getItem(key(logical));
      let value = old;
      if (existing !== null) {
        if (logical === 'yomu-outbox' || logical === 'yomu-pull-queue') {
          const field = logical === 'yomu-outbox' ? 'id' : 'chapter_id';
          value = JSON.stringify([...new Map([...JSON.parse(old), ...JSON.parse(existing)].map(item => [item[field], item])).values()]);
        } else if (logical === 'yomu-marks-outbox' || logical === 'yomu-device-chapters') {
          value = JSON.stringify({ ...JSON.parse(old), ...JSON.parse(existing) });
        } else continue;
      }
      await put(key(logical), value);
    }
    if (!native() && navigator.serviceWorker?.controller) await worker({ type: 'adopt-legacy', marks: JSON.parse(localStorage.getItem(key('yomu-device-chapters')) || '{}') });
    await put('yomu-legacy-imported', scope);
  }
  async function reconcile() {
    if (native() || !scope || !navigator.serviceWorker?.controller) return;
    const marks = JSON.parse(localStorage.getItem(key('yomu-device-chapters')) || '{}');
    for (const [chapter, mark] of Object.entries(marks)) {
      if (!(await worker({ type: 'saved-check', chapter, pages: typeof mark === 'number' ? mark : mark.pages }))) delete marks[chapter];
    }
    await put(key('yomu-device-chapters'), JSON.stringify(marks));
  }
  async function init(server) {
    const parsed = new URL(server);
    if (!['http:', 'https:'].includes(parsed.protocol) || parsed.username || parsed.password) throw new Error('Use an HTTP(S) server address without embedded credentials');
    parsed.search = ''; parsed.hash = '';
    if (!parsed.pathname.endsWith('/')) parsed.pathname += '/';
    base = parsed.href;
    const previous = localStorage.getItem('yomu-active-server');
    if (previous && previous !== base) {
      localStorage.removeItem('yomu-session'); localStorage.removeItem('yomu-media-token');
      if (native()) await native()('auth_sign_out');
    }
    await put('yomu-active-server', base);
    owner = JSON.parse(localStorage.getItem(ownerKey()) || 'null');
    try { owner = await who(); await put(ownerKey(), JSON.stringify(owner)); }
    catch { /* offline boot may use only the last explicitly identified owner */ }
    scope = identityScope(owner);
    if (navigator.serviceWorker?.controller) await worker({ type: 'set-scope' });
    // Today's auth mode cannot prove who owned an older unscoped journal.
    // Even shared-account installations recover legacy work explicitly.
    await reconcile();
    addEventListener('storage', event => {
      if (event.key === ownerKey() || event.key === 'yomu-active-server') {
        frozen = true; location.reload();
      }
    });
    navigator.serviceWorker?.addEventListener('controllerchange', () => {
      worker({ type: 'set-scope' }).catch(console.error);
    });
  }
  async function verify() {
    if (frozen) return false;
    const me = await who();
    if (identityScope(me) !== scope) {
      frozen = true;
      await put(ownerKey(), JSON.stringify(me));
      // Freeze the old tab before reloading; no stale task may acknowledge data.
      location.reload();
      return false;
    }
    return Boolean(scope);
  }
  async function logout() {
    frozen = true;
    owner = { mode: 'oidc', user: null };
    await put(ownerKey(), JSON.stringify(owner));
    const old = scope; scope = null;
    // Outboxes remain under their previous owner, not in signed-out storage.
    if (navigator.serviceWorker?.controller) await worker({ type: 'set-scope' });
    if (old) for (const stored of Object.keys(localStorage)) {
      if (stored.startsWith(`yomu-state:${old}:yomu-cache:`)) await put(stored, null);
    }
    await durable;
  }
  async function report() {
    const estimate = await navigator.storage?.estimate?.() || {};
    const saved = navigator.serviceWorker?.controller && scope ? await worker({ type: 'storage-report' }) : 0;
    const mib = n => ((n || 0) / 1048576).toFixed(1);
    return `Browser storage: ${mib(estimate.usage)} MiB used of ${mib(estimate.quota)} MiB; ${mib(saved)} MiB explicitly saved. Runtime cache is evictable; saved chapters are not automatically evicted.`;
  }
  window.YomuOffline = {
    init, key, worker, verify, logout, persist, owned, reconcile,
    active: () => Boolean(scope) && !frozen,
    scope: () => scope,
    legacy: () => !localStorage.getItem('yomu-legacy-imported') && legacyKeys().length > 0,
    pendingLegacy: () => {
      if (localStorage.getItem('yomu-legacy-imported')) return false;
      try { return JSON.parse(localStorage.getItem('yomu-outbox') || '[]').length > 0 || Object.keys(JSON.parse(localStorage.getItem('yomu-marks-outbox') || '{}')).length > 0; }
      catch { return true; }
    },
    importLegacy, report,
    exportRetained: () => JSON.stringify(Object.fromEntries(Object.keys(localStorage).filter(k => legacyKeys().includes(k) || k.startsWith(prefix())).map(k => [k, localStorage.getItem(k)])), null, 2),
    idle: () => durable,
  };
})();
