import { expect, test, type Page } from '@playwright/test';

async function signIn(page: Page, user: 'Alice' | 'Bob') {
  await page.goto('/');
  await page.getByRole('link', { name: 'Sign in' }).click();
  await expect(page.getByRole('heading', { name: 'Fixture identity provider' })).toBeVisible();
  await page.getByRole('link', { name: `Sign in as ${user}` }).click();
  await expect(page.getByText(`${user} Reader`)).toBeVisible();
}

async function signOut(page: Page) {
  await page.getByRole('button', { name: 'sign out' }).click();
  await expect(page.getByRole('link', { name: 'Sign in' })).toBeVisible();
}

async function openFixture(page: Page) {
  await page.goto('/search');
  await page.getByRole('searchbox').fill('Fixture');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  const card = page.locator('.manga-card').filter({ hasText: 'Fixture Farming' });
  await expect(card).toBeVisible();
  const track = card.getByRole('button', { name: 'track', exact: true });
  if (await track.isVisible().catch(() => false)) {
    await track.click();
    await expect(page.getByText('Added "Fixture Farming" to the library')).toBeVisible();
    await page.goto('/library');
    await page.locator('.manga-card').filter({ hasText: 'Fixture Farming' }).click();
  } else {
    await card.getByRole('link', { name: 'open' }).click();
  }
  await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
}

async function selectChapter(page: Page, title: string) {
  await page.getByTitle('Chapter actions').click();
  await page.getByRole('button', { name: 'Select', exact: true }).click();
  const row = page.locator('.chapter-item').filter({ hasText: title });
  await row.click({ position: { x: 5, y: 5 } });
  await expect(row).toHaveClass(/selected/);
  await page.getByTitle('Chapter actions').click();
  return row;
}

test.describe.serial('real browser journeys', () => {
  test('signs in, signs out, and switches accounts through the IdP stub', async ({ page }) => {
    await signIn(page, 'Alice');
    await signOut(page);
    await signIn(page, 'Bob');
    await expect(page.getByText('Bob Reader')).toBeVisible();
    await signOut(page);
  });

  test('tracks reading progress, read marks, and server/device downloads', async ({ page, context }) => {
    await signIn(page, 'Alice');
    await openFixture(page);

    let row = await selectChapter(page, 'Chapter 1');
    await page.getByRole('button', { name: 'Mark read', exact: true }).click();
    await expect(row).toHaveClass(/read/);

    row = await selectChapter(page, 'Chapter 1');
    await page.getByRole('button', { name: 'Download (both)', exact: true }).click();
    await expect(row).toHaveClass(/dl-both/, { timeout: 30_000 });

    await row.getByRole('link', { name: 'Chapter 1', exact: true }).click();
    await expect(page.getByTitle('Next page')).toBeVisible();
    await page.getByTitle('Next page').click();
    await page.goBack();
    await expect(page.getByRole('link', { name: 'Continue reading' })).toBeVisible();

    row = await selectChapter(page, 'Chapter 1');
    await page.getByRole('button', { name: 'Remove (server)', exact: true }).click();
    await expect(row).toHaveClass(/dl-local/);

    const sw = await page.evaluate(async () => {
      const registration = await navigator.serviceWorker.ready;
      await registration.update();
      return {
        active: registration.active?.state,
        script: registration.active?.scriptURL,
        controlled: Boolean(navigator.serviceWorker.controller),
      };
    });
    expect(sw).toEqual({
      active: 'activated',
      script: 'http://127.0.0.1:4711/sw.js',
      controlled: true,
    });

    await context.setOffline(true);
    await page.reload();
    await expect(page.getByRole('heading', { name: 'Fixture Farming' })).toBeVisible();
    await expect(page.getByText(/offline/).first()).toBeVisible();
    await row.getByRole('link', { name: 'Chapter 1', exact: true }).click();
    await expect(page.getByTitle('Next page')).toBeVisible();
    await context.setOffline(false);
  });

  test('keeps per-user read state isolated after an account change', async ({ page }) => {
    await signIn(page, 'Bob');
    await openFixture(page);
    const chapter = page.locator('.chapter-item').filter({ hasText: 'Chapter 1' });
    await expect(chapter).not.toHaveClass(/read/);
    await signOut(page);
    await signIn(page, 'Alice');
    await openFixture(page);
    await expect(page.locator('.chapter-item').filter({ hasText: 'Chapter 1' })).toHaveClass(/read/);
  });
});
