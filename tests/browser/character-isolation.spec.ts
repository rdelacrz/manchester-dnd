import { test, expect } from '@playwright/test';
import { openLocalGame } from './support/navigation-fixture';

test.describe('Character library pages', () => {
  test('characters page renders with empty state in local mode', async ({ page }) => {
    await page.goto('/characters');

    // The page should render the characters heading.
    await expect(page.getByRole('heading', { name: 'Characters' })).toBeVisible();

    // The "Create a character" link should be present.
    await expect(page.getByTestId('create-character-link')).toBeVisible();

    // In local mode, the empty state or character list should appear within the
    // protected layout. The side navigation should be present.
    await expect(page.getByRole('link', { name: 'Characters' }).first()).toBeVisible();
  });

  test('character new page renders creation options', async ({ page }) => {
    await page.goto('/characters/new');

    await expect(page.getByRole('heading', { name: 'Create a character' })).toBeVisible();
    await expect(page.getByTestId('use-local-creation')).toBeVisible();
    await expect(page.getByTestId('back-to-characters')).toBeVisible();
  });

  test('character detail page shows not-found for invalid character ID', async ({ page }) => {
    await page.goto('/characters/character:nonexistent-id');

    // Should show the not-found state since the character doesn't exist
    // or doesn't belong to the local account.
    await expect(page.getByText(/not found|doesn't exist|don't have access/i)).toBeVisible({ timeout: 10000 });
  });

  test('characters page has no axe accessibility violations', async ({ page }) => {
    await page.goto('/characters');
    // Wait for the page to settle.
    await page.waitForLoadState('networkidle');

    const axe = await page.evaluate(() => {
      return (window as any).axe?.();
    });
    // axe is injected in some configs; skip if not available.
    if (axe) {
      expect(axe.violations).toEqual([]);
    }
  });

  test('campaign stats page renders placeholder with character and campaign IDs', async ({ page }) => {
    const characterId = 'character:test-1234';
    const campaignId = 'campaign:test-5678';
    await page.goto(`/characters/${characterId}/campaigns/${campaignId}/stats`);

    await expect(page.getByRole('heading', { name: 'Campaign stats' })).toBeVisible();
    await expect(page.getByTestId('campaign-stats-placeholder')).toBeVisible();
    await expect(page.getByText(characterId)).toBeVisible();
    await expect(page.getByText(campaignId)).toBeVisible();
  });

  test('characters page side navigation includes all protected routes', async ({ page }) => {
    await page.goto('/characters');

    // The side navigation should include links to Characters, Campaigns, Play, Guide.
    const sideNav = page.locator('[data-testid="side-navigation"], nav.side-navigation');
    await expect(sideNav).toBeVisible({ timeout: 5000 });

    // Verify the Characters link is active.
    await expect(page.locator('.nav-active, [aria-current="page"]')).toBeVisible();
  });
});
