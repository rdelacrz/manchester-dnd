import { expect, test, type Page, type Request } from '@playwright/test';

import { ensureCampaignCreated } from './support/hero-fixture';

async function advanceCurrentStep(page: Page): Promise<Request | undefined> {
  const step = await page.locator('.hero-step').getAttribute('data-step');
  const choice: Record<string, string> = {
    CampaignTheme: 'Rainbound Borough',
    Concept: 'Canal Guardian',
    Rules: 'Human Fighter · Defense',
    AbilityScores: 'Steadfast',
    Background: 'Soldier',
    EquipmentAndSpells: 'Canal guard',
    Review: 'Confirm this review',
    Commit: 'Create and save hero',
  };

  if (step === 'Presentation') {
    await page.getByLabel('Name').fill('Asha Reed');
    await page.getByLabel('Pronouns').fill('she/they');
    await page.getByLabel('Appearance').fill('A moss-green coat, a brass lamp, and ink-stained gloves.');
    await page.getByLabel('Ideal').fill('Every crossing should be safe.');
    await page.getByLabel('Bond').fill('The rain wards sheltered my neighbours.');
    await page.getByLabel('Flaw').fill('I investigate warnings alone.');
    await page.getByLabel('Tone limits (comma-separated)').fill('No graphic gore, No cruelty to children');
    const request = page.waitForRequest((candidate) =>
      candidate.method() === 'POST' && candidate.url().includes('/api/advance_hero_creation'),
    );
    await page.getByRole('button', { name: 'Save presentation' }).click();
    return request;
  }

  const label = step ? choice[step] : undefined;
  expect(label, `known creator step: ${step}`).toBeTruthy();
  const request = page.waitForRequest((candidate) =>
    candidate.method() === 'POST' && candidate.url().includes('/api/advance_hero_creation'),
  );
  await page.getByRole('button', { name: new RegExp(`^${label}`) }).click();
  return request;
}

test('hero creation saves each step, survives refresh, and renders its derived sheet', async ({
  page,
}) => {
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await ensureCampaignCreated(page);
  await expect(page.locator('.hero-save-state')).not.toContainText('Loading');

  const begin = page.getByRole('button', { name: 'Begin guided creation' });
  if (await begin.isVisible()) {
    await begin.click();
    await expect(page.locator('.hero-step')).toHaveAttribute('data-step', 'CampaignTheme');
  }

  let capturedRequest: Request | undefined;
  let reloadedDraft = false;
  for (let transitions = 0; transitions < 10; transitions += 1) {
    if (await page.locator('.created-hero').isVisible()) break;
    await expect(page.locator('.hero-step')).toBeVisible();
    capturedRequest = (await advanceCurrentStep(page)) ?? capturedRequest;
    await expect(page.locator('.hero-save-state')).toContainText(
      /Choice saved|CharacterCreated committed/,
    );

    if (await page.locator('.created-hero').isVisible()) break;

    const nextStep = await page.locator('.hero-step').getAttribute('data-step');
    if (!reloadedDraft && nextStep === 'Background') {
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect(page.locator('.hero-step')).toHaveAttribute('data-step', 'Background');
      reloadedDraft = true;
    }
  }

  await expect(page.locator('.created-hero')).toBeVisible();
  await expect(page.locator('.created-hero')).toContainText(/Asha Reed|Level 1/);
  await expect(page.locator('.hero-sheet')).toContainText('HP');
  await expect(page.locator('.hero-sheet')).toContainText('AC');
  await expect(page.locator('.hero-sheet')).toContainText('Live encounter actions');
  await expect(page.locator('.level-preview')).toContainText('Level 2');

  const savedSheet = await page.locator('.hero-sheet').textContent();
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('.created-hero')).toBeVisible();
  await expect(page.locator('.hero-sheet')).toHaveText(savedSheet ?? '');

  if (capturedRequest) {
    const forged = JSON.parse(capturedRequest.postData() ?? '{}');
    forged.command.client_supplied_hit_points = 999;
    forged.command.client_supplied_proficiency = 99;
    const response = await page.request.post(capturedRequest.url(), {
      headers: {
        accept: 'application/json',
        'content-type': 'application/json',
        origin: new URL(page.url()).origin,
      },
      data: forged,
    });
    expect(response.status()).toBe(400);
    expect(await response.json()).toEqual({ code: 'invalid_server_input' });
  }
});
