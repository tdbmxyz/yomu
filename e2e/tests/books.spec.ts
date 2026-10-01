import { expect, test, type Page } from '@playwright/test';

async function signIn(page: Page) {
  await page.goto('/');
  await page.getByRole('link', { name: 'Sign in' }).click();
  await page.getByRole('link', { name: 'Sign in as Alice' }).click();
  await expect(page.getByText('Alice Reader', { exact: true })).toBeVisible();
}

async function publication(page: Page, title: string) {
  const response = await page.request.get('/api/v1/library');
  expect(response.ok()).toBeTruthy();
  const library = await response.json();
  const entry = library.find((entry: { title: string }) => entry.title === title);
  expect(entry).toBeTruthy();
  const detail = await (await page.request.get(`/api/v1/publications/${entry.id}`)).json();
  return detail;
}

test('Books shelf groups versions, decodes PDF covers and keeps format-specific progress', async ({ page }) => {
  await signIn(page);
  const library = await (await page.request.get('/api/v1/library')).json();
  expect(library.filter((entry: any) => entry.title === 'Fixture Editions')).toHaveLength(1);
  const entry = library.find((entry: any) => entry.title === 'Fixture Editions');
  expect(entry.kind).toBe('novels');
  expect(entry.chapter_count).toBe(2); // EPUB spine, NOT EPUB + PDF counts.
  expect(entry.editions.map((edition: any) => edition.format)).toEqual(['epub', 'pdf', 'mobi']);
  const epub = entry.editions[0], pdf = entry.editions[1], mobi = entry.editions[2];
  const standalone = library.find((entry: any) => entry.title === 'Fixture PDF');
  const cover = await page.request.get(`/api/v1/publications/${standalone.id}/cover`);
  expect(cover.ok()).toBeTruthy();
  expect(cover.headers()['content-type']).toBe('image/jpeg');
  expect((await cover.body()).subarray(0, 3)).toEqual(Buffer.from([0xff, 0xd8, 0xff]));
  await page.goto('/library');
  await page.locator('.kind-title').click();
  await page.getByRole('button', { name: 'Books', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Books', exact: true })).toBeVisible();
  await expect(page.locator('.manga-card').filter({ hasText: 'Fixture Editions' })).toHaveCount(1);
  const pdfCard = page.locator('.manga-card').filter({ hasText: 'Fixture PDF' });
  await expect.poll(() => pdfCard.locator('img').evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 100)).toBeTruthy();
  await page.locator('.manga-card').filter({ hasText: 'Fixture Editions' }).click();
  const picker = page.getByRole('combobox', { name: 'Book version', exact: true });
  await expect(picker).toHaveValue(epub.id);
  await picker.selectOption(pdf.id);
  await expect(page).toHaveURL(new RegExp(`/publications/${pdf.id}$`));
  await expect(picker).toHaveValue(pdf.id);
  await page.setViewportSize({ width: 375, height: 800 });
  expect(await page.locator('body').evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBeTruthy();
  await expect(picker).toBeVisible();
  await page.setViewportSize({ width: 1280, height: 720 });
  await page.getByRole('link', { name: 'Start reading', exact: true }).click();
  const canvas = page.frameLocator('.pdf-navigator').locator('canvas[aria-label]');
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
  await page.getByRole('button', { name: 'Next page', exact: true }).click();
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 2 of 2');
  await expect.poll(async () => (await (await page.request.get(`/api/v1/publications/${pdf.id}`)).json()).position?.page).toBe(1);
  await page.getByRole('link', { name: 'Back to publication', exact: true }).click();
  const active = (await (await page.request.get('/api/v1/library')).json()).find((e: any) => e.work_id === entry.work_id);
  expect(active.id).toBe(pdf.id);
  expect(active.chapter_count).toBe(1); // PDF document, not combined formats.
  await picker.selectOption(epub.id);
  await page.getByRole('link', { name: 'Start reading', exact: true }).click();
  const body = page.frameLocator('.epub-navigator').locator('body');
  await expect(page.frameLocator('.epub-navigator').getByRole('heading', { name: 'First section' })).toBeVisible();
  await body.evaluate(() => window.scrollTo(0, 900));
  await expect.poll(async () => (await (await page.request.get(`/api/v1/publications/${epub.id}`)).json()).position?.progression).toBeGreaterThan(0.1);
  await page.getByRole('link', { name: 'Back to publication', exact: true }).click();
  await picker.selectOption(pdf.id);
  await page.getByRole('link', { name: 'Continue reading', exact: true }).click();
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 2 of 2');
  await page.getByRole('link', { name: 'Back to publication', exact: true }).click();
  await picker.selectOption(mobi.id);
  await expect(page.getByRole('link', { name: /Start reading|Continue reading/ })).toHaveCount(0);
  await expect(page.getByText('1 unreadable file: mobi', { exact: true })).toBeVisible();
  const download = page.getByRole('link', { name: 'Download original', exact: true });
  await expect(download).toBeVisible();
  const original = await page.request.get((await download.getAttribute('href'))!);
  expect(original.ok()).toBeTruthy();
  expect(original.headers()['content-disposition']).toBe('attachment');
  expect(await original.text()).toContain('catalog-only MOBI placeholder');
  expect((await page.request.get(`/api/v1/publications/${mobi.id}/manifest`)).status()).toBe(422);
  await expect.poll(() => page.locator('.manga-head img').evaluate((img: HTMLImageElement) => img.complete && img.naturalWidth > 0)).toBeTruthy();
  expect((await page.request.post('/api/v1/library/rescan')).ok()).toBeTruthy();
  const rescanned = await (await page.request.get(`/api/v1/publications/${epub.id}`)).json();
  expect(rescanned.manga.work_id).toBe(entry.work_id);
  expect(rescanned.editions.map((e: any) => e.id)).toEqual(expect.arrayContaining([epub.id, pdf.id, mobi.id]));
  await page.getByRole('button', { name: 'Remove this version', exact: true }).click();
  await expect(page).toHaveURL(/\/$/);
  const remaining = await (await page.request.get(`/api/v1/publications/${epub.id}`)).json();
  expect(remaining.editions.map((e: any) => e.id)).toEqual(expect.arrayContaining([epub.id, pdf.id]));
  expect(remaining.editions).toHaveLength(2);
});

test('EPUB spine, isolated rendering and progression resume', async ({ page }) => {
  await signIn(page);
  const detail = await publication(page, 'Fixture EPUB');
  expect(detail.manga.kind).toBe('novels');
  expect(detail.chapters.map((unit: { title: string }) => unit.title)).toEqual(['First section', 'Second section']);
  const id = detail.manga.id, unit = detail.chapters[0].id;
  await page.goto(`/publications/${id}`);
  await page.getByRole('link', { name: /Start reading|Continue reading/ }).click();
  const frame = page.frameLocator('.epub-navigator');
  await expect(frame.getByRole('heading', { name: 'First section' })).toBeVisible();
  await expect.poll(() => frame.getByRole('img', { name: 'Fixture illustration' }).evaluate((image: HTMLImageElement) => image.complete && image.naturalWidth > 0)).toBeTruthy();
  // Reader colors override conflicting publisher !important colors, but
  // publisher typography and original illustration bytes remain intact.
  await expect(frame.locator('body')).toHaveCSS('background-color', 'rgb(22, 22, 22)');
  await expect(frame.getByRole('heading', { name: 'First section' })).toHaveCSS('color', 'rgb(238, 238, 238)');
  await expect(frame.getByRole('heading', { name: 'First section' })).toHaveCSS('letter-spacing', '0.5px');
  await expect(frame.locator('p').first()).toHaveCSS('color', 'rgb(238, 238, 238)');
  await expect(frame.locator('p').first()).toHaveCSS('text-shadow', 'none');
  await expect(frame.getByRole('link', { name: 'Go to second section' })).toHaveCSS('color', 'rgb(138, 197, 255)');
  await expect(frame.getByRole('img', { name: 'Fixture illustration' })).toHaveCSS('background-color', 'rgb(255, 255, 255)');
  await page.getByRole('button', { name: 'Reader options', exact: true }).click();
  await page.getByRole('combobox', { name: 'Reading colors' }).selectOption('paper');
  await expect(frame.locator('body')).toHaveCSS('background-color', 'rgb(255, 255, 255)');
  await expect(frame.locator('p').first()).toHaveCSS('color', 'rgb(32, 32, 32)');
  await page.getByRole('combobox', { name: 'Reading colors' }).selectOption('night');
  await expect(frame.locator('body')).not.toHaveAttribute('data-book-script', 'ran');
  await expect(page.locator('body')).not.toHaveAttribute('data-book-script', 'ran');
  const fontSize = await frame.locator('body').evaluate(body => getComputedStyle(body).fontSize);
  await page.getByTitle('Larger text', { exact: true }).click();
  await expect.poll(() => frame.locator('body').evaluate(body => parseFloat(getComputedStyle(body).fontSize))).toBeGreaterThan(parseFloat(fontSize));
  await page.getByTitle('Smaller text', { exact: true }).click();
  await expect(frame.locator('body')).toHaveCSS('font-size', fontSize);
  await frame.locator('body').evaluate(() => window.scrollTo(0, 900));
  await expect.poll(async () => {
    const detail = await (await page.request.get(`/api/v1/publications/${id}`)).json();
    return detail.position?.progression;
  }).toBeGreaterThan(0.1);
  const before = await frame.locator('body').evaluate(() => window.scrollY);
  await page.getByRole('link', { name: 'Back to publication', exact: true }).click();
  await page.getByRole('link', { name: 'Continue reading' }).click();
  await expect.poll(() => frame.locator('body').evaluate(() => window.scrollY)).toBeGreaterThan(before - 30);
  await page.reload();
  await expect.poll(() => frame.locator('body').evaluate(() => window.scrollY)).toBeGreaterThan(before - 30);
  await page.getByTitle('Contents', { exact: true }).selectOption(detail.chapters[1].id);
  await expect(frame.getByRole('heading', { name: 'Second section' })).toBeVisible();
  await expect(frame.locator('body')).toHaveCSS('background-color', 'rgb(22, 22, 22)');
  await frame.getByRole('link', { name: 'Return to first section' }).click();
  await expect(frame.getByRole('heading', { name: 'First section' })).toBeVisible();
  await expect(page.getByTitle('Contents', { exact: true })).toHaveValue(unit);
  await frame.getByRole('link', { name: 'Go to second section' }).click();
  await expect(frame.getByRole('heading', { name: 'Second section' })).toBeVisible();

  const manifest = await page.request.get(`/api/v1/publications/${id}/manifest`);
  expect(manifest.headers()['content-type']).toBe('application/webpub+json');
  const model = await manifest.json();
  expect(model.readingOrder[0].unit_id).toBe(unit);
  expect(model.resources.some((link: { rel?: string }) => link.rel === 'cover')).toBeTruthy();
});

test('PDF renders actual pages and restores page location', async ({ page }) => {
  await signIn(page);
  const detail = await publication(page, 'Fixture PDF');
  const id = detail.manga.id, unit = detail.chapters[0].id;
  expect(detail.manga.kind).toBe('novels');
  expect(detail.editions[0].format).toBe('pdf');
  expect(detail.chapters[0].page_count).toBe(2);
  // Simulate a scanner/renderer disagreement (malformed PDFs can have one).
  // The renderer must repair metadata using its actual displayed document.
  const seed = await page.request.put(`/api/v1/publications/${id}/position`, {
    data: { chapter_id: unit, page: 0, page_count: 3, device: 'e2e-scanner-count' },
  });
  expect(seed.ok()).toBeTruthy();
  await page.goto(`/read/${id}/${unit}?page=0`);
  // Canvas has no implicit image role; its accessible name and decoded pixel
  // content are both asserted below rather than trusting the host counter.
  const rendered = page.frameLocator('.pdf-navigator').locator('canvas[aria-label]');
  await expect(rendered).toHaveAttribute('aria-label', 'PDF page 1 of 2');
  await expect(page.frameLocator('.pdf-navigator').getByText('Fixture PDF - page one')).toBeVisible();
  const first = await rendered.evaluate((canvas: HTMLCanvasElement) => canvas.toDataURL());
  await page.getByRole('button', { name: 'Next page' }).click();
  await expect(rendered).toHaveAttribute('aria-label', 'PDF page 2 of 2');
  await expect(page.frameLocator('.pdf-navigator').getByText('Fixture PDF - page two')).toBeVisible();
  const second = await rendered.evaluate((canvas: HTMLCanvasElement) => canvas.toDataURL());
  expect(second).not.toBe(first);
  await expect(page.getByRole('button', { name: 'Next page' })).toBeDisabled();
  await page.getByRole('link', { name: 'Back to publication', exact: true }).click();
  await page.getByRole('link', { name: 'Continue reading' }).click();
  await expect(rendered).toHaveAttribute('aria-label', 'PDF page 2 of 2');
  await page.reload();
  await expect(rendered).toHaveAttribute('aria-label', 'PDF page 2 of 2');
  const resumed = await (await page.request.get(`/api/v1/publications/${id}`)).json();
  expect(resumed.chapters[0].page_count).toBe(2);
  const position = resumed.position;
  expect(position.page).toBe(1);
  expect(position.progression).toBeUndefined();
});

test('PDF zoom rerenders text and canvas, fits the viewport and keeps the page', async ({ page }) => {
  await signIn(page);
  const detail = await publication(page, 'Fixture PDF');
  await page.goto(`/read/${detail.manga.id}/${detail.chapters[0].id}?page=0`);
  const frame = page.frameLocator('.pdf-navigator');
  const canvas = frame.locator('canvas[aria-label]');
  await expect(canvas).toHaveAttribute('data-zoom-mode', 'fit-width');
  expect(await frame.locator('body').evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBeTruthy();
  const width = await canvas.evaluate(canvas => canvas.getBoundingClientRect().width);
  const text = frame.getByText('Fixture PDF - page one');
  const textWidth = await text.evaluate(text => text.getBoundingClientRect().width);
  const scale = Number(await canvas.getAttribute('data-zoom'));
  await page.getByRole('button', { name: 'Zoom in', exact: true }).click();
  await expect.poll(async () => Number(await canvas.getAttribute('data-zoom'))).toBeGreaterThan(scale);
  await expect.poll(() => canvas.evaluate(canvas => canvas.getBoundingClientRect().width)).toBeGreaterThan(width * 1.2);
  await expect.poll(() => text.evaluate(text => text.getBoundingClientRect().width)).toBeGreaterThan(textWidth * 1.2);
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
  await page.getByRole('button', { name: 'Zoom out', exact: true }).click();
  await expect.poll(async () => Number(await canvas.getAttribute('data-zoom'))).toBeCloseTo(scale, 3);
  await page.getByRole('combobox', { name: 'PDF zoom', exact: true }).selectOption('1.5');
  await expect(canvas).toHaveAttribute('data-zoom', '1.5');
  await page.getByRole('button', { name: 'Next page' }).click();
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 2 of 2');
  await expect(canvas).toHaveAttribute('data-zoom', '1.5');
  await page.setViewportSize({ width: 700, height: 800 });
  await expect(canvas).toHaveAttribute('data-zoom', '1.5');
  await page.getByRole('combobox', { name: 'PDF zoom', exact: true }).selectOption('fit-page');
  await expect(canvas).toHaveAttribute('data-zoom-mode', 'fit-page');
  const fits = await canvas.evaluate(canvas => {
    const rect = canvas.getBoundingClientRect();
    return rect.width <= innerWidth && rect.height <= innerHeight;
  });
  expect(fits).toBeTruthy();
  await page.getByRole('combobox', { name: 'PDF zoom', exact: true }).selectOption('fit-width');
  await expect(canvas).toHaveAttribute('data-zoom-mode', 'fit-width');
  const fitScale = Number(await canvas.getAttribute('data-zoom'));
  await frame.locator('body').hover();
  await page.keyboard.down('Control');
  await page.mouse.wheel(0, -100);
  await page.keyboard.up('Control');
  await expect.poll(async () => Number(await canvas.getAttribute('data-zoom'))).toBeGreaterThan(fitScale);
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 2 of 2');
});

// Dispatch real Chromium touch input, not synthetic DOM events. This exercises
// native scrolling, touch-action and pinch cancellation inside child frames.
async function touches(page: Page) {
  const session = await page.context().newCDPSession(page);
  await session.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 2 });
  type Point = { x: number; y: number; id: number };
  return {
    async gesture(start: Point[], end: Point[], cancel = false) {
      await session.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: start });
      for (let step = 1; step <= 8; step++) {
        const touchPoints = start.map((point, index) => ({
          id: point.id, x: point.x + (end[index].x - point.x) * step / 8,
          y: point.y + (end[index].y - point.y) * step / 8,
        }));
        await session.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints });
      }
      await session.send('Input.dispatchTouchEvent', { type: cancel ? 'touchCancel' : 'touchEnd', touchPoints: [] });
    },
    async close() { await session.detach(); },
  };
}

test('EPUB gestures preserve vertical reading, pinch text and swipe sections', async ({ page }) => {
  await signIn(page);
  await page.setViewportSize({ width: 600, height: 900 });
  const detail = await publication(page, 'Fixture EPUB');
  await page.goto(`/read/${detail.manga.id}/${detail.chapters[0].id}?progression=0`);
  const frame = page.frameLocator('.epub-navigator');
  await expect(frame.getByRole('heading', { name: 'First section' })).toBeVisible();
  const box = (await page.locator('.epub-navigator').boundingBox())!;
  const touch = await touches(page);
  try {
    await touch.gesture([{ x: 300, y: box.y + 450, id: 1 }], [{ x: 300, y: box.y + 200, id: 1 }]);
    await expect.poll(() => frame.locator('body').evaluate(() => scrollY)).toBeGreaterThan(50);
    await expect(page.getByTitle('Contents', { exact: true })).toHaveValue(detail.chapters[0].id);
    const font = await frame.locator('body').evaluate(body => parseFloat(getComputedStyle(body).fontSize));
    await touch.gesture([{ x: 260, y: box.y + 250, id: 1 }, { x: 340, y: box.y + 250, id: 2 }],
      [{ x: 200, y: box.y + 250, id: 1 }, { x: 400, y: box.y + 250, id: 2 }]);
    await expect.poll(() => frame.locator('body').evaluate(body => parseFloat(getComputedStyle(body).fontSize))).toBeGreaterThan(font * 1.5);
    await expect(page.getByTitle('Contents', { exact: true })).toHaveValue(detail.chapters[0].id);
    await touch.gesture([{ x: 450, y: box.y + 250, id: 1 }], [{ x: 150, y: box.y + 250, id: 1 }], true);
    await expect(page.getByTitle('Contents', { exact: true })).toHaveValue(detail.chapters[0].id);
    await touch.gesture([{ x: 450, y: box.y + 250, id: 1 }], [{ x: 150, y: box.y + 250, id: 1 }]);
    await expect(frame.getByRole('heading', { name: 'Second section' })).toBeVisible();
    // Start below the section's link: swiping on interactive links is
    // deliberately left to normal link interaction rather than navigation.
    await touch.gesture([{ x: 150, y: box.y + 350, id: 1 }], [{ x: 450, y: box.y + 350, id: 1 }]);
    await expect(frame.getByRole('heading', { name: 'First section' })).toBeVisible();
  } finally { await touch.close(); }
});

test('PDF touch swipes turn fitted pages, pinch zooms and enlarged pages pan', async ({ page }) => {
  await signIn(page);
  await page.setViewportSize({ width: 600, height: 900 });
  const detail = await publication(page, 'Fixture PDF');
  await page.goto(`/read/${detail.manga.id}/${detail.chapters[0].id}?page=0`);
  const frame = page.frameLocator('.pdf-navigator');
  const canvas = frame.locator('canvas[aria-label]');
  await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
  const box = (await page.locator('.pdf-navigator').boundingBox())!;
  const touch = await touches(page);
  try {
    await touch.gesture([{ x: 450, y: box.y + 200, id: 1 }], [{ x: 150, y: box.y + 200, id: 1 }]);
    await expect(canvas).toHaveAttribute('aria-label', 'PDF page 2 of 2');
    await touch.gesture([{ x: 150, y: box.y + 200, id: 1 }], [{ x: 450, y: box.y + 200, id: 1 }]);
    await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
    const scale = Number(await canvas.getAttribute('data-zoom'));
    await touch.gesture([{ x: 260, y: box.y + 250, id: 1 }, { x: 340, y: box.y + 250, id: 2 }],
      [{ x: 200, y: box.y + 250, id: 1 }, { x: 400, y: box.y + 250, id: 2 }]);
    await expect.poll(async () => Number(await canvas.getAttribute('data-zoom'))).toBeGreaterThan(scale * 1.5);
    await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
    const before = await frame.locator('body').evaluate(() => scrollX);
    await touch.gesture([{ x: 450, y: box.y + 350, id: 1 }], [{ x: 150, y: box.y + 350, id: 1 }]);
    await expect.poll(() => frame.locator('body').evaluate(() => scrollX)).toBeGreaterThan(before + 50);
    await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
    await page.getByRole('combobox', { name: 'PDF zoom', exact: true }).selectOption('fit-width');
    await expect(canvas).toHaveAttribute('data-zoom-mode', 'fit-width');
    await touch.gesture([{ x: 450, y: box.y + 200, id: 1 }], [{ x: 150, y: box.y + 200, id: 1 }], true);
    await expect(canvas).toHaveAttribute('aria-label', 'PDF page 1 of 2');
  } finally { await touch.close(); }
});

test('book chrome shares the reader back control, toggles without reflow and protects selection', async ({ page }) => {
  await signIn(page);
  // Controlled time proves double-click/selection do not trigger the pending
  // single-click timer; no sleeps or weakened negative assertions.
  await page.clock.install();
  for (const [title, navigator] of [['Fixture EPUB', '.epub-navigator'], ['Fixture PDF', '.pdf-navigator']]) {
    const detail = await publication(page, title);
    await page.goto(`/read/${detail.manga.id}/${detail.chapters[0].id}?page=0&progression=0`);
    const frame = page.frameLocator(navigator);
    if (title === 'Fixture EPUB') await expect(frame.getByRole('heading', { name: 'First section' })).toBeVisible();
    else await expect(frame.locator('canvas[aria-label]')).toHaveAttribute('aria-label', 'PDF page 1 of 2');
    const header = page.locator('.reader-top');
    const back = page.getByRole('link', { name: 'Back to publication', exact: true });
    await expect(back).toHaveAttribute('href', `/publications/${detail.manga.id}`);
    await expect(back.locator('svg')).toBeVisible();
    const bounds = (await page.locator(navigator).boundingBox())!;
    const scroll = await frame.locator('body').evaluate(() => scrollY);
    const tap = async () => {
      await page.mouse.click(bounds.x + bounds.width / 2, bounds.y + bounds.height * 0.6);
      await page.clock.fastForward(300);
    };
    await tap();
    await expect(header).toBeHidden();
    await expect(page.locator('.reader-bottom')).toBeHidden();
    expect(await page.locator(navigator).boundingBox()).toEqual(bounds);
    expect(await frame.locator('body').evaluate(() => scrollY)).toBe(scroll);
    await tap();
    await expect(header).toBeVisible();
    const text = title === 'Fixture EPUB' ? frame.getByText(/^Paragraph 1: /) : frame.getByText('Fixture PDF - page one');
    await text.dblclick();
    await expect.poll(() => frame.locator('body').evaluate(() => getSelection()?.toString().length || 0)).toBeGreaterThan(0);
    await page.clock.fastForward(300);
    await expect(header).toBeVisible();
    // The first click clearing an existing selection is not chrome intent.
    await tap();
    await expect(header).toBeVisible();
    await frame.locator('body').evaluate(() => getSelection()?.removeAllRanges());
    if (title === 'Fixture EPUB') {
      await frame.getByRole('link', { name: 'Jump to paragraph' }).click();
      await page.clock.fastForward(300);
      await expect(header).toBeVisible();
    }
    await page.evaluate(() => window.postMessage('yomu-chrome:toggle', '*'));
    await page.clock.fastForward(300);
    await expect(header).toBeVisible();
    await tap();
    await expect(header).toBeHidden();
    await page.keyboard.press('Escape');
    await expect(header).toBeVisible();
    const touch = await touches(page);
    try {
      const point = { x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height * 0.6, id: 1 };
      await touch.gesture([point], [point]);
      await page.clock.fastForward(300);
      await expect(header).toBeHidden();
      await touch.gesture([point], [point]);
      await page.clock.fastForward(300);
      await expect(header).toBeVisible();
    } finally { await touch.close(); }
    await back.click();
    await expect(page.getByRole('heading', { name: title, exact: true })).toBeVisible();
  }
});

test('EPUB width presets reflow without rewinding and persist on the device', async ({ page }) => {
  await signIn(page);
  const detail = await publication(page, 'Fixture EPUB');
  await page.goto(`/read/${detail.manga.id}/${detail.chapters[0].id}?progression=0`);
  const frame = page.frameLocator('.epub-navigator');
  const body = frame.locator('body');
  await expect(frame.getByRole('heading', { name: 'First section' })).toBeVisible();
  const original = await body.evaluate(body => body.getBoundingClientRect().width);
  await body.evaluate(() => scrollTo(0, 900));
  await expect.poll(() => body.evaluate(() => scrollY)).toBeGreaterThan(850);
  const progression = await body.evaluate(() => scrollY / (document.documentElement.scrollHeight - innerHeight));
  await page.getByRole('button', { name: 'Reader options', exact: true }).click();
  const width = page.getByRole('combobox', { name: 'Reading width', exact: true });
  await width.selectOption('narrow');
  await expect.poll(() => body.evaluate(body => body.getBoundingClientRect().width)).toBeLessThan(original);
  await expect.poll(() => body.evaluate(() => scrollY / (document.documentElement.scrollHeight - innerHeight))).toBeCloseTo(progression, 2);
  await width.selectOption('wide');
  await expect.poll(() => body.evaluate(body => body.getBoundingClientRect().width)).toBeGreaterThan(original);
  await expect.poll(() => body.evaluate(() => scrollY / (document.documentElement.scrollHeight - innerHeight))).toBeCloseTo(progression, 2);
  await expect(frame.getByRole('heading', { name: 'First section' })).toHaveCSS('letter-spacing', '0.5px');
  await page.reload();
  await expect.poll(() => body.evaluate(body => body.getBoundingClientRect().width)).toBeGreaterThan(original);
  await page.getByRole('button', { name: 'Reader options', exact: true }).click();
  await expect(width).toHaveValue('wide');
  await page.getByTitle('Contents', { exact: true }).selectOption(detail.chapters[1].id);
  await expect(frame.getByRole('heading', { name: 'Second section' })).toBeVisible();
  await expect.poll(() => body.evaluate(body => body.getBoundingClientRect().width)).toBeGreaterThan(original);
  await page.getByRole('button', { name: 'Reader options', exact: true }).click();
  await expect(width).toHaveValue('wide');
  await width.selectOption('full');
  await expect.poll(() => body.evaluate(body => body.getBoundingClientRect().width)).toBeGreaterThan(original * 1.3);
  await page.setViewportSize({ width: 360, height: 800 });
  await expect.poll(() => body.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBeTruthy();
  expect(await page.locator('.reader-bottom').evaluate(element => element.getBoundingClientRect().right <= innerWidth)).toBeTruthy();
  const menu = (await page.locator('.publication-reader-options').boundingBox())!;
  const controls = (await page.locator('.reader-bottom').boundingBox())!;
  expect(menu.x + menu.width).toBeLessThanOrEqual(360);
  expect(menu.y + menu.height).toBeLessThanOrEqual(controls.y);
});

test('publication resources require identity or a valid media capability', async ({ page, playwright }) => {
  await signIn(page);
  const detail = await publication(page, 'Fixture EPUB');
  const token = (await (await page.request.get('/api/v1/auth/media-token')).json()).token;
  const anonymous = await playwright.request.newContext({ baseURL: 'http://127.0.0.1:4711' });
  const path = `/api/v1/publications/${detail.manga.id}`;
  expect((await anonymous.get(`${path}/manifest`)).status()).toBe(401);
  const cookieResource = await page.request.get(`${path}/resources/-/OPS/style.css`, { headers: { Origin: 'null' } });
  expect(cookieResource.status()).toBe(200);
  expect(cookieResource.headers()['access-control-allow-origin']).toBeUndefined();
  expect((await anonymous.get(`${path}/resources/-/OPS/style.css`)).status()).toBe(401);
  expect((await anonymous.get(`${path}/resources/invalid/OPS/style.css`)).status()).toBe(401);
  const resource = await anonymous.get(`${path}/resources/${token}/OPS/style.css`, { headers: { Origin: 'null' } });
  expect(resource.status()).toBe(200);
  expect(resource.headers()['content-type']).toBe('text/css');
  expect(resource.headers()['access-control-allow-origin']).toBe('*');
  // Encoded traversal reaches the handler as an archive path, not a normal
  // filesystem lookup. It must never resolve to a server configuration file.
  expect((await anonymous.get(`${path}/resources/${token}/OPS/%2E%2E%2Fyomu.toml`)).status()).not.toBe(200);
  const pdf = await publication(page, 'Fixture PDF');
  const pdfToken = (await (await page.request.get('/api/v1/auth/media-token')).json()).token;
  const range = await anonymous.get(`/api/v1/publications/${pdf.manga.id}/resources/${pdfToken}/Fixture%20PDF.pdf`, { headers: { Range: 'bytes=0-7' } });
  expect(range.status()).toBe(206);
  expect((await range.body()).toString()).toBe('%PDF-1.4');
  await anonymous.dispose();
});
