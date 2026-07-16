import { expect, test, type Page } from '@playwright/test';

import { ensureHeroCreated } from './support/hero-fixture';

async function replaceWithFreshCampaign(page: Page): Promise<void> {
  const panel = page.locator('#campaigns');
  await expect(panel.getByRole('status')).not.toContainText('Loading');

  const create = panel.getByRole('button', { name: 'Create local campaign' });
  if (await create.isVisible()) {
    await create.click();
    await expect(panel.locator('.campaign-library-card')).toBeVisible();
    return;
  }

  const endPlay = panel.getByRole('button', { name: 'End play session' });
  if (await endPlay.isVisible()) {
    await endPlay.click();
    await expect(panel.getByRole('button', { name: 'Start play session' })).toBeVisible();
  }

  const restoreArchive = panel.getByRole('button', { name: 'Restore archive' });
  if (!(await restoreArchive.isVisible())) {
    await panel.getByRole('button', { name: 'Archive' }).click();
    await expect(restoreArchive).toBeVisible();
  }
  await panel.getByRole('button', { name: 'Prepare permanent delete' }).click();
  await expect(panel.locator('.delete-confirmation')).toBeVisible();
  await panel.getByRole('button', { name: 'Confirm permanent delete' }).click();
  await expect(create).toBeVisible();
  await create.click();
  await expect(panel.locator('.campaign-library-card')).toBeVisible();
}

async function socialSnapshot(page: Page) {
  return page.locator('.social-panel').evaluate((panel) => ({
    state: panel.querySelector('.social-state')?.textContent ?? '',
    notice: panel.querySelector('.social-notice')?.textContent ?? '',
    button: panel.querySelector('.social-action')?.textContent ?? '',
  }));
}

test('the authored social scene saves trusted mechanics, rejects forgery, and leads into exploration', async ({
  page,
}) => {
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await replaceWithFreshCampaign(page);
  await ensureHeroCreated(page);

  const panel = page.locator('.social-panel');
  await expect(panel).toContainText('Trust objective: 0/1');
  await expect(panel).toContainText('Soot tide: 0/4');
  await expect(panel).toContainText('Indifferent');
  await expect(panel).toContainText('Social turn1');

  const requestPromise = page.waitForRequest(
    (request) =>
      request.method() === 'POST' &&
      request.url().includes('/api/attempt_social_interaction'),
  );
  await page
    .getByRole('button', { name: 'Ask Elin about the sealed rain gate' })
    .click();
  const legalRequest = await requestPromise;
  const legalBody = legalRequest.postData() ?? '';
  expect(legalBody).not.toMatch(
    /difficulty|difficulty_class|ability|skill|proficiency|roll|objective|clock|attitude/,
  );

  await expect(panel.locator('.social-notice')).toContainText('Saved social roll');
  await expect(panel.locator('.social-notice')).toContainText('mapped DC 15');
  await expect(panel).toContainText('Soot tide: 1/4');
  await expect(panel).toContainText('Social turn2');
  await expect(panel).toContainText(/Trust objective: (completed|failed)/);
  await expect(panel).toContainText(/Friendly|Hostile/);
  await expect(
    panel.getByRole('button', { name: 'Conversation already saved' }),
  ).toBeDisabled();

  const saved = await socialSnapshot(page);
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('.social-panel .social-notice')).toContainText(
    'Saved social roll',
  );
  await expect.poll(() => socialSnapshot(page)).toEqual(saved);

  const forged = new URLSearchParams(legalBody);
  forged.set('command[idempotency_key]', crypto.randomUUID());
  forged.set('command[difficulty_class]', '1');
  forged.set('command[roll]', '20');
  forged.set('command[attitude]', 'friendly');
  const forgedResponse = await page.request.post(legalRequest.url(), {
    headers: {
      accept: 'application/json',
      'content-type': 'application/x-www-form-urlencoded',
      origin: new URL(page.url()).origin,
    },
    data: forged.toString(),
  });
  expect(forgedResponse.status()).toBe(400);
  expect(await forgedResponse.json()).toEqual({ code: 'invalid_server_input' });
  await expect.poll(() => socialSnapshot(page)).toEqual(saved);

  await page.locator('.roll-button').click();
  await expect(page.locator('.roll-readout')).toContainText('Saved roll');
  await expect(page.locator('.encounter-scene')).toBeVisible();
  await expect(panel.getByRole('button', { name: 'Conversation already saved' })).toBeDisabled();
});
