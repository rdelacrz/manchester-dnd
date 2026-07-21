import { expect, type Page } from '@playwright/test';

/**
 * Transitional route fixture for the multi-page rewrite.
 *
 * The public home page at `/` is now introductory-only. The full local game
 * lives at the transitional `/play` route until authenticated routes replace it.
 */
export const localGameRoute = process.env.PLAYWRIGHT_GAME_ROUTE ?? '/play';

export async function openLocalGame(page: Page): Promise<void> {
  const response = await page.goto(localGameRoute, { waitUntil: 'domcontentloaded' });
  expect(response?.status()).toBe(200);
}

export async function expectLegacyGameRegions(page: Page): Promise<void> {
  await expect(page.getByTestId('introduction-region')).toBeVisible();
  await expect(page.getByTestId('campaign-region')).toBeVisible();
  await expect(page.getByTestId('character-region')).toBeVisible();
  await expect(page.getByTestId('gameplay-region')).toBeVisible();
}

/**
 * The public home page at `/` must contain introductory content.
 */
export async function expectPublicHomePage(page: Page): Promise<void> {
  await expect(page.getByTestId('introduction-region')).toBeVisible();
  await expect(page.getByTestId('campaign-region')).not.toBeVisible();
  await expect(page.getByTestId('character-region')).not.toBeVisible();
  await expect(page.getByTestId('gameplay-region')).not.toBeVisible();
}
