import { expect, test } from '@playwright/test';

import { ensureCampaignCreated, ensureHeroCreated } from './support/hero-fixture';

test('local campaign lifecycle survives play, archive, delete, and canonical restore', async ({
  page,
}) => {
  const response = await page.goto('/', { waitUntil: 'domcontentloaded' });
  expect(response?.status()).toBe(200);
  await ensureCampaignCreated(page);
  await ensureHeroCreated(page);

  const panel = page.locator('#campaigns');
  await expect(panel.getByRole('heading', { name: 'Save, resume, and export' })).toBeVisible();
  await expect(panel.locator('.campaign-library-card')).toBeVisible();

  const endPlay = panel.getByRole('button', { name: 'End play session' });
  if (await endPlay.isVisible()) {
    await endPlay.click();
    await expect(panel.getByRole('button', { name: 'Start play session' })).toBeVisible();
  }

  await panel.getByRole('button', { name: 'Start play session' }).click();
  await expect(endPlay).toBeVisible();
  const runeAction = page.locator('.roll-button');
  if (await runeAction.isEnabled()) {
    await runeAction.click();
    await expect(page.locator('.roll-readout')).toContainText('Saved roll');
  }
  await endPlay.click();
  await expect(panel.getByRole('button', { name: 'Archive' })).toBeVisible();

  await panel.getByRole('button', { name: 'Load history' }).click();
  await expect(panel.locator('[role="status"]')).toContainText('History rendered from saved audits only');

  await panel.getByRole('button', { name: 'Build/update private recap' }).click();
  await expect(panel.locator('[role="status"]')).toContainText(
    'Private recap saved with its committed-audit provenance.',
  );
  await expect(panel.locator('.private-recap')).toContainText('Turn 1');
  const savedRecap = await panel.locator('.private-recap pre').textContent();
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(panel.locator('.campaign-library-card')).toBeVisible();
  await panel.getByRole('button', { name: 'Load saved private recap' }).click();
  await expect(panel.locator('.private-recap pre')).toHaveText(savedRecap ?? '');

  await panel.getByRole('button', { name: 'Canonical export' }).click();
  await expect(panel.locator('.private-export-field textarea')).toHaveValue(/^\{/);

  await panel.getByRole('button', { name: 'Archive' }).click();
  await expect(panel.getByRole('button', { name: 'Restore archive' })).toBeVisible();
  await panel.getByRole('button', { name: 'Prepare permanent delete' }).click();
  await expect(panel.locator('.delete-confirmation')).toBeVisible();
  const canonicalExport = await panel.locator('.private-export-field textarea').inputValue();
  expect(canonicalExport).toContain('"schema_version":1');
  expect(canonicalExport).toContain('"private_recaps":[');

  await panel.getByRole('button', { name: 'Confirm permanent delete' }).click();
  await expect(panel.getByRole('button', { name: 'Create local campaign' })).toBeVisible();

  await panel.locator('.restore-export summary').click();
  await panel.locator('.restore-export textarea').fill(canonicalExport);
  await panel.getByRole('button', { name: 'Validate and restore' }).click();
  await expect(panel.locator('.campaign-library-card')).toBeVisible();
  await expect(panel.getByRole('button', { name: 'Restore archive' })).toBeVisible();

  // The pre-delete export intentionally preserved archive state. Restore it
  // explicitly so subsequent browser journeys see an active campaign.
  await panel.getByRole('button', { name: 'Restore archive' }).click();
  await expect(panel.getByRole('button', { name: 'Start play session' })).toBeVisible();
  await panel.getByRole('button', { name: 'Load saved private recap' }).click();
  await expect(panel.locator('.private-recap pre')).toHaveText(savedRecap ?? '');
});
