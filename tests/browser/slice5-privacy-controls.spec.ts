import AxeBuilder from '@axe-core/playwright';
import { expect, test } from '@playwright/test';

test('private-inspiration controls are always visible and fail closed by default', async ({
  page,
}) => {
  const response = await page.goto('/', { waitUntil: 'domcontentloaded' });
  expect(response?.status()).toBe(200);
  await page.waitForLoadState('networkidle');
  await page.waitForTimeout(100);
  const gameplayStatus = page.locator('.roll-demo .save-status');
  await expect(gameplayStatus).not.toContainText('Loading');
  if ((await gameplayStatus.textContent())?.includes('Campaign unavailable')) {
    const create = page
      .locator('#campaigns')
      .getByRole('button', { name: 'Create local campaign' });
    await expect(create).toBeVisible();
    await create.click();
    await expect(page.locator('#campaigns .campaign-library-card')).toBeVisible();
    await page.reload({ waitUntil: 'domcontentloaded' });
  }
  await expect(gameplayStatus).toContainText(/saved revision \d+/);

  const panel = page.locator('#privacy');
  await expect(
    panel.getByRole('heading', { name: 'Real stories stay under your control.' }),
  ).toBeVisible();
  await expect(panel.getByRole('status')).toContainText(
    'Private inspiration is disabled for this installation.',
  );
  await expect(
    panel.getByRole('button', { name: 'Pause private generation' }),
  ).toBeDisabled();
  await expect(
    panel.getByRole('button', { name: 'Resume private generation' }),
  ).toBeDisabled();
  await expect(
    panel.getByRole('button', { name: 'Disable all inspiration' }),
  ).toBeDisabled();
  for (const name of [
    'Veil current passage',
    'Veto this source',
    'Disable this category',
    'Report a privacy issue',
  ]) {
    await expect(panel.getByRole('button', { name })).toBeVisible();
    await expect(panel.getByRole('button', { name })).toBeDisabled();
  }

  const results = await new AxeBuilder({ page }).include('#privacy').analyze();
  expect(results.violations).toEqual([]);
});
