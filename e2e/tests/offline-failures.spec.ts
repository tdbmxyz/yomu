import { expect, test, type Page } from '@playwright/test';

async function signIn(page: Page, name: 'Alice' | 'Bob') {
  await page.goto('/');
  await page.getByRole('link', { name: 'Sign in', exact: true }).click();
  await page.getByRole('link', { name: `Sign in as ${name}` }).click();
  await expect(page.getByText(`${name} Reader`)).toBeVisible();
}
async function fixture(page: Page) {
  await page.goto('/search');
  await page.getByRole('searchbox').fill('Fixture');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  const card = page.locator('.manga-card').filter({ hasText: 'Fixture Farming' });
  await expect(card).toBeVisible();
  const track = card.getByRole('button', { name: 'track', exact: true });
  if (await track.isVisible()) {
    await track.click();
    await expect(page.getByText('Added "Fixture Farming" to the library')).toBeVisible();
    await page.goto('/library');
    await page.locator('.manga-card').filter({ hasText: 'Fixture Farming' }).click();
  } else await card.getByRole('link', { name: 'open', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
}
async function stored(page: Page, key: string) {
  return page.evaluate(key => {
    const api = (window as any).YomuOffline;
    return JSON.parse(localStorage.getItem(api.key(key)) || 'null');
  }, key);
}
async function seedEvent(page: Page, legacy = false) {
  return page.evaluate(async legacy => {
    const library = await (await fetch('/api/v1/library')).json();
    const id = library[0].id;
    const detail = await (await fetch(`/api/v1/publications/${id}`)).json();
    const event = { id: crypto.randomUUID(), manga_id: id, chapter_id: detail.chapters[0].id, page: 1, device: 'e2e-offline', at: new Date().toISOString() };
    localStorage.setItem(legacy ? 'yomu-outbox' : (window as any).YomuOffline.key('yomu-outbox'), JSON.stringify([event]));
    return event;
  }, legacy);
}

test.beforeEach(async ({ request }) => {
  await request.post('/__test/control', { data: { reset: true } });
});

test('keeps queued work across 429 and reconnect, without duplicate progress', async ({ page, request }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page);
  await request.post('/__test/control', { data: { fault: { path: '/api/v1/progress/events', method: 'POST', status: 429 } } });
  const rejected = page.waitForResponse(r => r.url().endsWith('/progress/events') && r.status() === 429);
  await page.reload();
  await rejected;
  expect(await stored(page, 'yomu-outbox')).toEqual([event]);
  // Retry has a 5s backoff and a 5s polling cadence, not an immediate loop.
  await expect.poll(() => stored(page, 'yomu-outbox'), { timeout: 20_000 }).toEqual([]);
  const journal = await page.evaluate(async () => (await fetch('/api/v1/progress/events')).json());
  expect(journal.events.filter((e: any) => e.id === event.id)).toHaveLength(1);
});

test('account switch quarantines pending work and never submits it as Bob', async ({ page, request }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page);
  await page.getByRole('button', { name: 'sign out', exact: true }).click();
  await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
  await signIn(page, 'Bob');
  expect(await stored(page, 'yomu-outbox')).toBeNull();
  const bob = await page.evaluate(async () => (await fetch('/api/v1/progress/events')).json());
  expect(bob.events.some((e: any) => e.id === event.id)).toBe(false);
  await page.getByRole('button', { name: 'sign out', exact: true }).click();
  await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
  // Hold Alice's first retry so restored ownership can be observed.
  await request.post('/__test/control', { data: { fault: { path: '/api/v1/progress/events', method: 'POST', status: 429 } } });
  await signIn(page, 'Alice');
  await expect.poll(() => stored(page, 'yomu-outbox')).toEqual([event]);
  await expect.poll(() => stored(page, 'yomu-outbox'), { timeout: 20_000 }).toEqual([]);
});

test('a new read mark survives acknowledgement of the previous state', async ({ page, request }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page);
  await page.evaluate(id => {
    const key = (window as any).YomuOffline.key('yomu-marks-outbox');
    localStorage.setItem(key, JSON.stringify({ [id]: true }));
  }, event.chapter_id);
  await request.post('/__test/control', { data: { fault: { path: '/api/v1/units/mark', method: 'POST', delay: 1500 } } });
  const inFlight = page.waitForRequest(r => r.url().endsWith('/units/mark') && r.method() === 'POST');
  await page.reload();
  await inFlight;
  await page.evaluate(id => localStorage.setItem((window as any).YomuOffline.key('yomu-marks-outbox'), JSON.stringify({ [id]: false })), event.chapter_id);
  await expect.poll(() => stored(page, 'yomu-marks-outbox')).toEqual({ [event.chapter_id]: false });
  await expect.poll(() => stored(page, 'yomu-marks-outbox'), { timeout: 20_000 }).toEqual({});
  const detail = await page.evaluate(async id => (await fetch(`/api/v1/publications/${id}`)).json(), event.manga_id);
  expect(detail.chapters.find((c: any) => c.id === event.chapter_id).read).toBe(false);
});

test('a real worker upgrade preserves verified saved pages for offline boot', async ({ page, context, request }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page);
  // Exercise the same worker save protocol as the UI, without a download queue race.
  await page.evaluate(async id => {
    const api = (window as any).YomuOffline;
    await api.worker({ type: 'save-begin', chapter: id });
    for (let n = 0; n < 3; n++) await api.worker({ type: 'save-page', chapter: id, page: n, url: `/api/v1/units/${id}/pages/${n}` });
    await api.worker({ type: 'save-finish', chapter: id, pages: 3 });
  }, event.chapter_id);
  await request.post('/__test/control', { data: { workerRevision: 2 } });
  await page.evaluate(async () => {
    const changed = new Promise<void>(resolve => navigator.serviceWorker.addEventListener('controllerchange', () => resolve(), { once: true }));
    await (await navigator.serviceWorker.ready).update();
    await changed;
  });
  await context.setOffline(true);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
  await page.evaluate(async id => {
    const response = await fetch(`/api/v1/units/${id}/pages/0`);
    if (!response.ok) throw new Error(`offline image: ${response.status}`);
    const image = await createImageBitmap(await response.blob());
    if (!image.width) throw new Error('image did not decode');
    image.close();
  }, event.chapter_id);
});

test('quota failure and interrupted saves never publish a complete chapter', async ({ page, context }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const row = page.locator('.chapter-item').filter({ hasText: 'Chapter 1' });
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await row.click({ position: { x: 5, y: 5 } });
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Download (server)', exact: true }).click();
  await expect(row).toHaveClass(/dl-server/, { timeout: 20_000 });
  const event = await seedEvent(page);
  const worker = context.serviceWorkers()[0];
  await worker.evaluate(() => {
    const original = Cache.prototype.put;
    (self as any).restoreCachePut = () => { Cache.prototype.put = original; };
    Cache.prototype.put = async function(request, response) {
      if (String(typeof request === 'string' ? request : request.url).includes('/pages/')) {
        throw new DOMException('fixture quota exhausted', 'QuotaExceededError');
      }
      return original.call(this, request, response);
    };
  });
  const error = await page.evaluate(async id => {
    const api = (window as any).YomuOffline;
    await api.worker({ type: 'save-begin', chapter: id });
    try { await api.worker({ type: 'save-page', chapter: id, page: 0, url: `/api/v1/units/${id}/pages/0` }); }
    catch (error) { return String(error); }
    return null;
  }, event.chapter_id);
  expect(error).toMatch(/quota/i);
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await row.click({ position: { x: 5, y: 5 } });
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Download (local)', exact: true }).click();
  await expect(page.getByText(/Local save failed:.*quota/)).toBeVisible();
  await expect(row).not.toHaveClass(/dl-local|dl-both/);
  await worker.evaluate(() => (self as any).restoreCachePut());
  await page.reload();
  const saved = await page.evaluate(async id => (window as any).YomuOffline.worker({ type: 'saved-check', chapter: id, pages: 3 }), event.chapter_id);
  expect(saved).toBe(false);
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  await row.click({ position: { x: 5, y: 5 } });
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Download (local)', exact: true }).click();
  await expect(row).toHaveClass(/dl-local|dl-both/);
  expect(await page.evaluate(async id => (window as any).YomuOffline.worker({ type: 'saved-check', chapter: id, pages: 3 }), event.chapter_id)).toBe(true);
  // Leave the shared fixture server as we found it for the download-both journey.
  expect(await page.evaluate(async id => (await fetch('/api/v1/units/remove-downloads', {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ chapter_ids: [id] }),
  })).ok, event.chapter_id)).toBe(true);
});

test('413 splits a batch without dropping either event', async ({ page, request }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page);
  const second = { ...event, id: crypto.randomUUID(), page: 2 };
  await page.evaluate(events => localStorage.setItem((window as any).YomuOffline.key('yomu-outbox'), JSON.stringify(events)), [event, second]);
  await request.post('/__test/control', { data: { fault: { path: '/api/v1/progress/events', method: 'POST', status: 413 } } });
  await page.reload();
  await expect.poll(() => stored(page, 'yomu-outbox'), { timeout: 20_000 }).toEqual([]);
  const journal = await page.evaluate(async () => (await fetch('/api/v1/progress/events')).json());
  for (const id of [event.id, second.id]) expect(journal.events.filter((e: any) => e.id === id)).toHaveLength(1);
});

test('unowned legacy work is retained until explicit recovery into its original account', async ({ page }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page, true);
  await page.getByRole('button', { name: 'sign out', exact: true }).click();
  await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
  await signIn(page, 'Bob');
  expect(await stored(page, 'yomu-outbox')).toBeNull();
  const bob = await page.evaluate(async () => (await fetch('/api/v1/progress/events')).json());
  expect(bob.events.some((e: any) => e.id === event.id)).toBe(false);
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('yomu-outbox')!))).toEqual([event]);
  await page.getByRole('button', { name: 'sign out', exact: true }).click();
  await expect(page.getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
  await signIn(page, 'Alice');
  await page.goto('/more');
  await expect(page.getByText(/Legacy offline data has no recorded owner/)).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  const reloaded = page.waitForEvent('framenavigated', frame => frame === page.mainFrame());
  await page.getByRole('button', { name: 'Import legacy offline data into this account' }).click();
  await reloaded;
  await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible();
  await expect.poll(() => page.evaluate(() => localStorage.getItem('yomu-legacy-imported'))).toBeTruthy();
  await expect.poll(() => stored(page, 'yomu-outbox'), { timeout: 20_000 }).toEqual([]);
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('yomu-outbox')!))).toEqual([event]);
  const alice = await page.evaluate(async () => (await fetch('/api/v1/progress/events')).json());
  expect(alice.events.filter((e: any) => e.id === event.id)).toHaveLength(1);
});

test('other tabs change identity too, and stale worker handshakes cannot restore Alice', async ({ page, context }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const aliceScope = await page.evaluate(() => (window as any).YomuOffline.scope());
  const other = await context.newPage();
  await other.goto('/');
  await expect(other.getByText('Alice Reader')).toBeVisible();
  await page.getByRole('button', { name: 'sign out', exact: true }).click();
  await expect(other.getByRole('link', { name: 'Sign in', exact: true })).toBeVisible();
  await signIn(page, 'Bob');
  await expect(other.getByText('Bob Reader')).toBeVisible();
  const result = await other.evaluate(scope => new Promise<any>(resolve => {
    const channel = new MessageChannel();
    channel.port1.onmessage = ({ data }) => resolve(data);
    navigator.serviceWorker.controller!.postMessage({ type: 'set-scope', scope, base: 'http://127.0.0.1:4711/' }, [channel.port2]);
  }), aliceScope);
  expect(result.error).toMatch(/Account changed/);
  const cachesLeft = await page.evaluate(() => caches.keys());
  expect(cachesLeft.some(name => name.startsWith(`yomu-private:${aliceScope}`) || name.startsWith(`yomu-saved:${aliceScope}:`))).toBe(false);
  await expect(other.getByText('Bob Reader')).toBeVisible();
});

test('interrupted replacement keeps the previous complete chapter', async ({ page }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const event = await seedEvent(page);
  await page.evaluate(async id => {
    const api = (window as any).YomuOffline;
    await api.worker({ type: 'save-begin', chapter: id });
    for (let n = 0; n < 3; n++) await api.worker({ type: 'save-page', chapter: id, page: n, url: `/api/v1/units/${id}/pages/${n}` });
    await api.worker({ type: 'save-finish', chapter: id, pages: 3 });
    await api.worker({ type: 'save-begin', chapter: id });
    await api.worker({ type: 'save-page', chapter: id, page: 0, url: `/api/v1/units/${id}/pages/0` });
  }, event.chapter_id);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
  expect(await page.evaluate(async id => (window as any).YomuOffline.worker({ type: 'saved-check', chapter: id, pages: 3 }), event.chapter_id)).toBe(true);
  const incomplete = await page.evaluate(async id => {
    try { await (window as any).YomuOffline.worker({ type: 'save-finish', chapter: id, pages: 3 }); }
    catch (error) { return String(error); }
    return null;
  }, event.chapter_id);
  expect(incomplete).toMatch(/missing page/);
});

test('first upgrade retries initialization when a legacy worker has no RPC handler', async ({ page, request }) => {
  await signIn(page, 'Alice');
  await fixture(page);
  const retained = await seedEvent(page, true);
  await request.post('/__test/control', { data: { legacyWorker: true } });
  await page.evaluate(async () => {
    const changed = new Promise<void>(resolve => navigator.serviceWorker.addEventListener('controllerchange', () => resolve(), { once: true }));
    await (await navigator.serviceWorker.ready).update();
    await changed;
    await caches.delete('yomu-control-v1');
  });
  // The new UI is now controlled by a legacy worker which cannot answer init.
  await page.reload();
  await request.post('/__test/control', { data: { legacyWorker: false, workerRevision: 3 } });
  await page.evaluate(async () => (await navigator.serviceWorker.ready).update());
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible({ timeout: 15_000 });
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem('yomu-outbox')!))).toEqual([retained]);
  expect(await stored(page, 'yomu-outbox')).toBeNull();
});
