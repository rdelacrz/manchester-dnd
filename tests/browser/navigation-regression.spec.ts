import { expect, test } from '@playwright/test';

import {
  expectLegacyGameRegions,
  localGameRoute,
  openLocalGame,
} from './support/navigation-fixture';

test('the transitional local route retains every pre-rewrite game region', async ({ page }) => {
  await openLocalGame(page);
  await expectLegacyGameRegions(page);

  await expect(page.getByTestId('introduction-region')).toContainText(
    'Your city. Your stories. A realm remade.',
  );
  await expect(page.getByTestId('campaign-region')).toContainText('Campaign');
  await expect(page.getByTestId('character-region')).toContainText(/hero|character/i);
  await expect(page.getByTestId('gameplay-region')).toContainText('Trust the roll');
  expect(new URL(page.url()).pathname).toBe(localGameRoute);
});