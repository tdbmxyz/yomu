import { expect, test, type Page } from '@playwright/test';

async function signIn(page: Page) {
  await page.goto('/more');
  await page.getByRole('main').getByRole('link', { name: 'Sign in', exact: true }).click();
  await page.getByRole('link', { name: 'Sign in as Alice' }).click();
  await page.waitForURL(url => url.origin === 'http://127.0.0.1:4711');
  await page.goto('/more');
  await expect(page.getByRole('main').getByText('Alice Reader')).toBeVisible();
}

async function marathon(page: Page) {
  await page.goto('/search');
  await page.getByRole('searchbox').fill('Fixture');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  const card = page.locator('.browse-card').filter({ hasText: 'Fixture Marathon' });
  await expect(card).toBeVisible();
  if (await card.getByRole('button', { name: 'track', exact: true }).isVisible()) {
    await card.getByRole('button', { name: 'track', exact: true }).click();
    await expect(page.getByText('Added "Fixture Marathon" to the library')).toBeVisible();
    await page.goto('/library');
    await page.locator('.manga-card').filter({ hasText: 'Fixture Marathon' }).click();
  } else {
    await card.getByRole('link', { name: 'open', exact: true }).click();
  }
  await expect(page.getByRole('heading', { name: 'Fixture Marathon' })).toBeVisible();
  return page.getByRole('link', { name: 'Chapter 1', exact: true });
}

async function decoded(image: ReturnType<Page['locator']>) {
  await expect.poll(() => image.evaluate((img: HTMLImageElement) =>
    img.complete && img.naturalWidth > 0)).toBe(true);
}

test.beforeEach(async ({ request }) => {
  await request.post('/__test/control', { data: { reset: true } });
});

test('mobile source icons and previews work without tracking; pagination keeps existing cards', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  await page.getByRole('link', { name: /Sources$/ }).click();
  await decoded(page.locator('.source-card').filter({ hasText: 'Fixture Source' }).locator('.source-icon img'));
  let sourceRequests = 0;
  page.on('request', req => { if (new URL(req.url()).pathname === '/api/v1/sources') sourceRequests++; });
  await page.getByRole('link', { name: /Fixture Source/ }).click();
  const card = page.locator('.browse-card').filter({ hasText: 'Fixture Farming' });
  await expect(card).toBeVisible();
  expect(sourceRequests).toBe(0);
  const rejected = await page.request.get('/api/v1/sources/fixture/preview', {
    params: { key: 'http://127.0.0.1:4811/not-a-catalog-key' },
  });
  expect(rejected.status()).toBe(404);
  const libraryBefore = await page.evaluate(async () => (await (await fetch('/api/v1/library')).json()).length);
  let previews = 0;
  page.on('request', req => { if (new URL(req.url()).pathname.endsWith('/preview')) previews++; });
  expect(previews).toBe(0);
  await card.getByRole('link', { name: 'Fixture Farming', exact: true }).click();
  await expect(page).toHaveURL(/\/sources\/fixture\/title\?key=/);
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
  await expect(page.getByText('A deterministic comic for browser tests.')).toBeVisible();
  await expect(page.locator('dd').filter({ hasText: /^3$/ })).toBeVisible();
  await expect(page.getByText('2026-01-03')).toBeVisible();
  await expect(page.locator('.browse-grid')).toHaveCount(0);
  expect(previews).toBe(1);
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
  expect(previews).toBe(2);
  await page.getByRole('link', { name: 'Back to catalog' }).click();
  await expect(card).toBeVisible();
  expect(await page.evaluate(async () => (await (await fetch('/api/v1/library')).json()).length)).toBe(libraryBefore);
  await card.evaluate(el => el.setAttribute('data-retained', 'yes'));
  await page.getByRole('button', { name: 'Load more', exact: true }).click();
  await expect(page.locator('.browse-card')).toHaveCount(3);
  await expect(page.getByText('Fixture Orchard', { exact: true })).toBeVisible();
  await expect(card).toHaveAttribute('data-retained', 'yes');
  await page.getByRole('button', { name: 'Load more', exact: true }).click();
  await expect(page.getByText('End of the listing.')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test('reopening a source title fetches live details again and falls back when refused', async ({ page, request }) => {
  const setBlocked = async (blocked: boolean) => {
    const res = await request.post(`http://127.0.0.1:4811/__fixture/orchard-blocked?value=${blocked}`);
    expect(res.status()).toBe(204);
  };
  await signIn(page);
  const openOrchard = async () => {
    await page.getByRole('button', { name: 'Load more', exact: true }).click();
    const card = page.locator('.browse-card').filter({ hasText: 'Fixture Orchard' });
    await expect(card.locator('.cover-empty')).toBeVisible();
    await expect(card.locator('img')).toBeHidden();
    await card.getByRole('link', { name: 'Fixture Orchard', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Fixture Orchard' })).toBeVisible();
  };
  try {
    await setBlocked(true);
    await page.goto('/sources/fixture');
    // Wait for page one before asking for page two.
    await expect(page.getByRole('link', { name: 'Fixture Farming', exact: true })).toBeVisible();
    await openOrchard();
    await expect(page.getByText(/requires a browser check.*Showing available catalog information/)).toBeVisible();
    const sourceLink = page.getByRole('link', { name: 'Open on source site' });
    await expect(sourceLink).toHaveAttribute('href', 'http://127.0.0.1:4811/manga/orchard');
    const popupPromise = page.waitForEvent('popup');
    await sourceLink.click();
    const popup = await popupPromise;
    await expect(popup).toHaveURL('http://127.0.0.1:4811/manga/orchard');
    await popup.close();
    await expect(page.getByText('No summary available.')).toBeVisible();
    await expect(page.locator('dd')).toHaveText(['Unavailable', 'Unavailable']);

    await setBlocked(false);
    await page.getByRole('link', { name: 'Back to catalog' }).click();
    await expect(page.getByRole('link', { name: 'Fixture Farming', exact: true })).toBeVisible();
    await openOrchard();
    await expect(page.getByText('A freshly loaded orchard summary.')).toBeVisible();
    await page.getByRole('button', { name: 'track', exact: true }).click();
    await expect(page.getByRole('link', { name: 'open', exact: true })).toBeVisible();

    await setBlocked(true);
    await page.reload();
    await expect(page.getByText(/Showing saved library information/)).toBeVisible();
    await expect(page.getByText('A freshly loaded orchard summary.')).toBeVisible();
    await expect(page.locator('dd')).toHaveText(['3', '2026-01-03']);
    await page.getByRole('link', { name: 'open', exact: true }).click();
    await expect(page.getByRole('heading', { name: 'Fixture Orchard' })).toBeVisible();
    await expect(page.getByRole('link', { name: 'Open on source site' })).toHaveAttribute('href', 'http://127.0.0.1:4811/manga/orchard');
    const lastChecked = page.getByText(/^Last successful chapter check:/);
    const previousCheck = await lastChecked.textContent();
    await page.getByRole('button', { name: 'Check for new chapters' }).click();
    await expect(page.getByText(/requires a browser check/)).toBeVisible();
    await expect(lastChecked).toHaveText(previousCheck!);
    await expect(page.getByRole('link', { name: 'Chapter 1', exact: true })).toBeVisible();
  } finally {
    await setBlocked(false);
  }
});

test('optional FlareSolverr restores challenged details, refresh and reader images', async ({ page, request }) => {
  await signIn(page);
  const stats = async () => (await request.get('http://127.0.0.1:4811/__fixture/solver-stats')).json();
  const before = await stats();
  await page.goto('/sources/solver-fixture');
  await page.getByRole('link', { name: 'Solver Comic', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Solver Comic' })).toBeVisible();
  await expect(page.locator('dd')).toHaveText(['3', '2026-01-03']);
  await expect(page.getByText(/couldn't provide live details|requires a browser check/)).toHaveCount(0);
  await page.getByRole('button', { name: 'track', exact: true }).click();
  await page.getByRole('link', { name: 'open', exact: true }).click();
  await page.getByRole('button', { name: 'Check for new chapters' }).click();
  await expect(page.getByText('No new chapters', { exact: true })).toBeVisible();
  await page.getByRole('link', { name: 'Chapter 1', exact: true }).click();
  await decoded(page.locator('.reader-panel').nth(1).locator('img'));
  await expect.poll(async () => (await stats()).images).toBeGreaterThan(before.images);
  expect((await stats()).solves - before.solves).toBe(1);
});

test('search preview returns to the submitted query', async ({ page }) => {
  await signIn(page);
  await page.goto('/search');
  const search = page.getByRole('searchbox');
  await search.fill('Fixture');
  await search.press('Enter');
  await page.getByRole('link', { name: 'Fixture Farming', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
  await page.getByRole('link', { name: 'Back to search' }).click();
  await expect(search).toHaveValue('Fixture');
  await expect(page.getByRole('link', { name: 'Fixture Farming', exact: true })).toBeVisible();
});

test('switching listings during an in-flight request loads the selected listing', async ({ page, request }) => {
  await signIn(page);
  await request.post('/__test/control', { data: { fault: { path: '/api/v1/sources/fixture/browse', delay: 1200 } } });
  const popular = page.waitForRequest(req => req.url().includes('/browse?sort=popular'));
  await page.goto('/sources/fixture');
  await popular;
  const latest = page.waitForResponse(res => res.url().includes('/browse?sort=latest'));
  await page.getByRole('button', { name: 'Latest', exact: true }).click();
  await latest;
  await expect(page.locator('.browse-card').filter({ hasText: 'Fixture Farming' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Load more', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Latest', exact: true })).toHaveClass(/active/);
});

test('a failed image renews its media credential, with an explicit retry after repeated failure', async ({ page, context, request }) => {
  await signIn(page);
  // Reproduce the shell's bearer-only API requests: images cannot silently
  // fall back to a browser session cookie when their capability is expired.
  const session = (await context.cookies()).find(cookie => cookie.name === 'yomu_session')!.value;
  await page.evaluate(session => {
    localStorage.setItem('yomu-session', session);
    localStorage.setItem('yomu-media-token', 'expired');
  }, session);
  await context.clearCookies();
  await page.clock.install();
  await page.reload();
  const link = await marathon(page);
  const href = (await link.getAttribute('href'))!;
  const unit = new URL(href, 'http://localhost').pathname.split('/').at(-1)!;
  const path = `/api/v1/units/${unit}/pages/0`;
  for (let i = 0; i < 2; i++) {
    await request.post('/__test/control', { data: { fault: { path, status: 401 } } });
  }
  const renewed = page.waitForResponse(res => res.url().endsWith('/auth/media-token') && res.ok());
  await link.click();
  await renewed;
  const retry = page.getByRole('button', { name: 'Image unavailable — retry' });
  await expect(retry).toBeVisible();
  await retry.click();
  await decoded(page.locator('.reader-panel').nth(1).locator('img'));
  await expect(retry).toBeHidden();
  await expect(page).toHaveURL(new RegExp(unit));
  await expect(page.locator('.reader-panel').nth(1).locator('img')).toHaveAttribute('src', /[?&]mt=/);
  expect(await page.evaluate(() => localStorage.getItem('yomu-media-token'))).not.toBe('expired');
  const image = page.locator('.reader-panel').nth(1).locator('img');
  const before = await image.getAttribute('src');
  const periodicRenewal = page.waitForResponse(res => res.url().endsWith('/auth/media-token') && res.ok());
  await page.clock.fastForward(30 * 60 * 1000);
  await periodicRenewal;
  await decoded(image);
  await expect(image).not.toHaveAttribute('src', before!);
  await expect(page).toHaveURL(new RegExp(unit));
});

test('continuous reading decodes images across twelve chapters with bounded mounted chapters', async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  const link = await marathon(page);
  await link.click();
  await page.getByTitle('Reader options').click();
  await page.getByTitle('Toggle paged / vertical').click();
  await expect(page.locator('.reader-scroll')).toBeVisible();
  // Release the opening anchor exactly as a physical keyboard gesture does.
  await page.keyboard.press('Shift');
  for (let chapter = 1; chapter <= 12; chapter++) {
    const segment = page.locator('.strip-chapter').filter({ has: page.locator('.strip-chapter-break', { hasText: new RegExp(`^Chapter ${chapter}$`) }) });
    await expect(segment).toBeAttached();
    const last = segment.locator('img').last();
    await last.evaluate(el => el.scrollIntoView({ block: 'center' }));
    await decoded(last);
    await expect.poll(() => page.locator('.strip-chapter').count()).toBeLessThanOrEqual(5);
  }
  await expect(page.locator('.strip-chapter-break', { hasText: /^Chapter 12$/ })).toBeVisible();
  await expect(page.locator('.reader-image-retry')).toHaveCount(0);
});
