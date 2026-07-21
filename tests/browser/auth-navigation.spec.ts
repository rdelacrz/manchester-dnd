import AxeBuilder from '@axe-core/playwright';
import { expect, type Page, test } from '@playwright/test';

import {
  expectPublicHomePage,
  localGameRoute,
  openLocalGame,
} from './support/navigation-fixture';

async function expectPlayerNavigationVisible(page: Page) {
  const sideNavigation = page.getByTestId('side-navigation');
  await expect(sideNavigation).toBeVisible();

  if ((page.viewportSize()?.width ?? 0) <= 768) {
    await expect(sideNavigation.getByRole('button', { name: /open menu/i })).toBeVisible();
  } else {
    await expect(page.getByRole('navigation', { name: 'Player navigation' })).toBeVisible();
  }
}

test('public home page has intro content but no game regions or side navigation', async ({
  page,
}) => {
  const response = await page.goto('/', { waitUntil: 'domcontentloaded' });
  expect(response?.status()).toBe(200);

  await expect(page.getByTestId('introduction-region')).toBeVisible();
  await expect(page.getByTestId('campaign-region')).not.toBeVisible();
  await expect(page.getByTestId('character-region')).not.toBeVisible();
  await expect(page.getByTestId('gameplay-region')).not.toBeVisible();

  // No side navigation on the public home page.
  await expect(page.getByRole('navigation', { name: 'Player navigation' })).not.toBeVisible();

  // Signed-out top links include login and sign-up.
  await expect(page.getByRole('link', { name: 'Log in', exact: false })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Create an account', exact: true })).toBeVisible();
});

test('login page renders an accessible form with link to sign-up', async ({ page }) => {
  await page.goto('/login', { waitUntil: 'domcontentloaded' });

  await expect(page.getByRole('heading', { name: /log in to your account/i })).toBeVisible();
  await expect(page.getByLabel('Email')).toBeVisible();
  await expect(page.getByLabel('Password')).toBeVisible();
  await expect(page.getByRole('button', { name: /log in/i })).toBeVisible();
  await expect(page.getByRole('link', { name: /sign up/i })).toHaveAttribute('href', '/signup');

  // Password field has password-manager-compatible autocomplete.
  const passwordInput = page.locator('input[type="password"]');
  await expect(passwordInput).toHaveAttribute('autocomplete', 'current-password');
  const emailInput = page.locator('input[type="email"]');
  await expect(emailInput).toHaveAttribute('autocomplete', 'email');
});

test('sign-up page renders an accessible form with link to login', async ({ page }) => {
  await page.goto('/signup', { waitUntil: 'domcontentloaded' });

  await expect(page.getByRole('heading', { name: /create an account/i })).toBeVisible();
  await expect(page.getByLabel('Email')).toBeVisible();
  await expect(page.getByLabel('Display name')).toBeVisible();
  await expect(page.getByLabel('Password')).toBeVisible();
  await expect(page.getByRole('button', { name: /sign up/i })).toBeVisible();
  await expect(page.getByRole('link', { name: /log in/i })).toHaveAttribute('href', '/login');

  // New password field uses new-password autocomplete.
  const passwordInput = page.locator('input[type="password"]');
  await expect(passwordInput).toHaveAttribute('autocomplete', 'new-password');
});

test('local game remains accessible at the transitional /play route', async ({ page }) => {
  await openLocalGame(page);
  await expect(page.getByTestId('introduction-region')).toBeVisible();
  await expect(page.getByTestId('campaign-region')).toBeVisible();
  await expect(page.getByTestId('character-region')).toBeVisible();
  await expect(page.getByTestId('gameplay-region')).toBeVisible();
  expect(new URL(page.url()).pathname).toBe(localGameRoute);
});

test('protected routes render side navigation in local mode', async ({ page }) => {
  // In local mode, the compatibility principal is always authenticated.
  // Protected routes should render the authenticated layout with side navigation.
  await page.goto('/characters', { waitUntil: 'domcontentloaded' });

  // Wait for the suspense to resolve and content to appear.
  await expectPlayerNavigationVisible(page);
  await expect(page.getByRole('heading', { name: 'Characters' })).toBeVisible();
  await expect(
    page.locator('[data-testid="characters-placeholder"], [data-testid="characters-empty"]'),
  ).toBeVisible();

  // Side navigation links are present.
  await expect(page.getByTestId('side-navigation').locator('a[href="/characters"]')).toHaveCount(1);
  await expect(page.getByTestId('side-navigation').locator('a[href="/campaigns"]')).toHaveCount(1);

  // Logout button is present.
  await expect(page.getByRole('button', { name: /logout/i })).toBeVisible();
});

test('campaigns protected route renders in local mode', async ({ page }) => {
  await page.goto('/campaigns', { waitUntil: 'domcontentloaded' });

  await expectPlayerNavigationVisible(page);
  await expect(page.getByRole('heading', { name: 'Campaigns' })).toBeVisible();
  await expect(page.getByTestId('campaigns-placeholder')).toBeVisible();
});

test('protected routes have no axe accessibility violations', async ({ page }) => {
  await page.goto('/characters', { waitUntil: 'domcontentloaded' });
  const results = await new AxeBuilder({ page }).analyze();
  expect(results.violations).toEqual([]);
});

test('auth pages have no axe accessibility violations', async ({ page }) => {
  await page.goto('/login', { waitUntil: 'domcontentloaded' });
  const loginResults = await new AxeBuilder({ page }).analyze();
  expect(loginResults.violations).toEqual([]);

  await page.goto('/signup', { waitUntil: 'domcontentloaded' });
  const signupResults = await new AxeBuilder({ page }).analyze();
  expect(signupResults.violations).toEqual([]);
});
