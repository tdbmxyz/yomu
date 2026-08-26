import { expect, test } from '@playwright/test';

// Seed for Playwright's planner/generator agents: it proves both fixture
// services are up and leaves the browser at yomu's unauthenticated shell.
test('agent seed', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('link', { name: 'Sign in' })).toBeVisible();
});
