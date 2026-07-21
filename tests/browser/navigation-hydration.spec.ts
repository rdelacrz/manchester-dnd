import { expect, test } from '@playwright/test';

test('hydrated navigation replaces protected route content', async ({ page }) => {
  const browserErrors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') {
      browserErrors.push(message.text());
    }
  });
  page.on('pageerror', (error) => browserErrors.push(error.message));

  await page.goto('/characters');
  await expect(page.getByRole('heading', { name: 'Characters' })).toBeVisible();
  await expect(page.locator('body')).toHaveAttribute('data-hydrated', 'true');
  expect(browserErrors).toEqual([]);
  await page.evaluate(() => {
    (window as Window & { __navigationDocument?: string }).__navigationDocument =
      'same-document';
  });

  const playerNavigation = page.getByRole('navigation', {
    name: 'Player navigation',
  });
  const campaignsLink = playerNavigation.getByRole('link', {
    name: 'Campaigns',
  });
  if (!(await campaignsLink.isVisible())) {
    await page.getByRole('button', { name: 'Open menu' }).click();
  }
  await campaignsLink.click();

  await expect(page).toHaveURL('/campaigns');
  await expect(page.getByRole('heading', { name: 'Campaigns' })).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (window as Window & { __navigationDocument?: string })
            .__navigationDocument,
      ),
    )
    .toBe('same-document');
  expect(browserErrors).toEqual([]);
});
