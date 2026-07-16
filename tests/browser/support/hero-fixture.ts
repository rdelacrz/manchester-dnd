import { expect, type Page } from '@playwright/test';

// Shared setup for browser journeys that begin after campaign or hero creation.

const stepChoice: Record<string, string> = {
  CampaignTheme: 'Rainbound Borough',
  Concept: 'Canal Guardian',
  Rules: 'Human Fighter · Defense',
  AbilityScores: 'Steadfast',
  Background: 'Soldier',
  EquipmentAndSpells: 'Canal guard',
  Review: 'Confirm this review',
  Commit: 'Create and save hero',
};

/** Idempotently satisfies the campaign-sealing gate for browser journeys that need gameplay. */
export async function ensureCampaignCreated(page: Page): Promise<void> {
  const campaignPanel = page.locator('#campaigns');
  await expect(campaignPanel.getByRole('status')).not.toContainText('Loading');

  const create = campaignPanel.getByRole('button', { name: 'Create local campaign' });
  if (await create.isVisible()) {
    await create.click();
  }

  await expect(campaignPanel.locator('.campaign-library-card')).toBeVisible();
  await expect(page.locator('.roll-demo .save-status')).toContainText(
    /saved revision \d+/,
  );
}

/** Idempotently satisfies the campaign and hero gates for browser gameplay journeys. */
export async function ensureHeroCreated(page: Page): Promise<void> {
  await ensureCampaignCreated(page);
  await expect(page.locator('.hero-save-state')).not.toContainText('Loading');
  if (await page.locator('.created-hero').isVisible()) return;

  const begin = page.getByRole('button', { name: 'Begin guided creation' });
  if (await begin.isVisible()) {
    await begin.click();
    await expect(page.locator('.hero-step')).toHaveAttribute('data-step', 'CampaignTheme');
  }

  for (let transition = 0; transition < 10; transition += 1) {
    if (await page.locator('.created-hero').isVisible()) break;
    const stepNode = page.locator('.hero-step');
    await expect(stepNode).toBeVisible();
    const step = await stepNode.getAttribute('data-step');
    if (step === 'Presentation') {
      await page.getByLabel('Name').fill('Asha Reed');
      await page.getByLabel('Pronouns').fill('she/they');
      await page
        .getByLabel('Appearance')
        .fill('A moss-green coat, a brass lamp, and ink-stained gloves.');
      await page.getByLabel('Ideal').fill('Every crossing should be safe.');
      await page.getByLabel('Bond').fill('The rain wards sheltered my neighbours.');
      await page.getByLabel('Flaw').fill('I investigate warnings alone.');
      await page
        .getByLabel('Tone limits (comma-separated)')
        .fill('No graphic gore, No cruelty to children');
      await page.getByRole('button', { name: 'Save presentation' }).click();
    } else {
      const label = step ? stepChoice[step] : undefined;
      expect(label, `known creator step: ${step}`).toBeTruthy();
      await page.getByRole('button', { name: new RegExp(`^${label}`) }).click();
    }
    await expect(page.locator('.hero-save-state')).toContainText(
      /Choice saved|CharacterCreated committed/,
    );
  }

  await expect(page.locator('.created-hero')).toBeVisible();
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('.created-hero')).toBeVisible();
  await expect(page.locator('.roll-demo .save-status')).toContainText(
    /saved revision \d+/,
  );
}
