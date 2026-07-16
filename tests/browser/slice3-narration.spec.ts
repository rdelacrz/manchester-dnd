import { expect, test } from '@playwright/test';

import { ensureHeroCreated } from './support/hero-fixture';

test.skip(
  process.env.PLAYWRIGHT_TEXT_BACKEND !== 'fake',
  'requires the dedicated deterministic-fake browser server',
);

async function mechanicsSnapshot(page: import('@playwright/test').Page) {
  return page.locator('.encounter-scene').evaluate((scene) => ({
    metadata: scene.querySelector('.encounter-meta')?.textContent ?? '',
    combatants: scene.querySelector('.combatants')?.textContent ?? '',
    actions: scene.querySelector('.encounter-actions')?.textContent ?? '',
    rolls: [...scene.querySelectorAll('.roll-record')].map(
      (record) => record.textContent ?? '',
    ),
  }));
}

test('fake narration versions retry without recommitting mechanics and replay a lost response', async ({
  page,
}) => {
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  const campaignPanel = page.locator('#campaigns');
  await expect(page.locator('.roll-demo .save-status')).toContainText(
    /saved revision \d+/,
  );
  // The gameplay loader bootstraps the fixed local campaign in this isolated
  // database. Refresh the independently loaded lifecycle list after that race
  // so both panels observe the same durable row.
  await campaignPanel
    .getByRole('button', { name: 'Reload campaign list' })
    .click();
  await expect(campaignPanel.locator('.campaign-library-card')).toBeVisible();
  await ensureHeroCreated(page);

  const runeAction = page.locator('.roll-button');
  if (await runeAction.isEnabled()) {
    await runeAction.click();
    await expect(page.locator('.roll-readout')).toContainText('Saved roll');
  }
  await expect(page.locator('.encounter-scene')).toBeVisible();
  await expect(page.locator('.encounter-actions .encounter-action').first()).toBeEnabled();

  // Let the server resolve and commit the initial typed command, then discard
  // the response. The component retains the exact command and the next request
  // reconstructs the committed result from durable receipts before stale-
  // revision validation or another provider call.
  let discardedInitialResponse = false;
  const typedIntentEndpoint = '**/api/submit_typed_player_intent*';
  await page.route(typedIntentEndpoint, async (route) => {
    if (discardedInitialResponse) {
      await route.continue();
      return;
    }
    discardedInitialResponse = true;
    const response = await route.fetch();
    expect(response.ok()).toBeTruthy();
    await route.abort('failed');
  });
  await page.getByLabel('Describe another action').fill('Begin initiative.');
  await page.getByRole('button', { name: 'Interpret against legal actions' }).click();
  await expect(page.locator('.encounter-notice')).toContainText('response was interrupted');
  await expect(page.getByLabel('Describe another action')).toBeDisabled();
  await page.unroute(typedIntentEndpoint);
  await page.getByRole('button', { name: 'Recover interrupted action' }).click();
  await expect(page.locator('.typed-gm-result')).toContainText('Saved interpretation');
  await expect(page.locator('.narration-history summary')).toContainText('(1/3)');
  await expect(page.locator('.narration-history')).toContainText('Version 1 — selected');
  const mechanicsAfterCommit = await mechanicsSnapshot(page);

  // Let the server commit version two, then deliberately discard its response.
  // The component must retain the exact command key and recover that version on
  // the next click instead of spending version three.
  let discardedResponse = false;
  const regenerationEndpoint = '**/api/regenerate_narration_presentation*';
  await page.route(regenerationEndpoint, async (route) => {
    if (discardedResponse) {
      await route.continue();
      return;
    }
    discardedResponse = true;
    const response = await route.fetch();
    expect(response.ok()).toBeTruthy();
    await route.abort('failed');
  });
  await page
    .getByRole('button', { name: 'Retry narration presentation only' })
    .click();
  await expect(page.locator('.encounter-notice')).toContainText(
    'response was interrupted',
  );
  await page.unroute(regenerationEndpoint);

  await page
    .getByRole('button', { name: 'Recover interrupted narration retry' })
    .click();
  await expect(page.locator('.encounter-notice')).toContainText(
    'Narration version 2 selected',
  );
  await expect(page.locator('.narration-history summary')).toContainText('(2/3)');
  await expect(page.locator('.narration-history li')).toHaveCount(2);
  expect(await mechanicsSnapshot(page)).toEqual(mechanicsAfterCommit);

  await page
    .getByRole('button', { name: 'Retry narration presentation only' })
    .click();
  await expect(page.locator('.encounter-notice')).toContainText(
    'Narration version 3 selected',
  );
  await expect(page.locator('.narration-history summary')).toContainText('(3/3)');
  await expect(page.locator('.narration-history li')).toHaveCount(3);
  await expect(
    page.getByRole('button', { name: 'Presentation retry limit reached' }),
  ).toBeDisabled();
  expect(await mechanicsSnapshot(page)).toEqual(mechanicsAfterCommit);
});
