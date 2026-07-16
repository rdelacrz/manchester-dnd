import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page } from '@playwright/test';

import { ensureCampaignCreated, ensureHeroCreated } from './support/hero-fixture';

const baseURL =
  process.env.PLAYWRIGHT_BASE_URL ??
  `http://${process.env.PLAYWRIGHT_ADDRESS ?? '127.0.0.1:6789'}`;
const shellMarkers = [
  'Manchester Arcana',
  'Your city. Your stories. A realm remade.',
  'Inspect the viaduct runes',
] as const;

function collectBrowserProblems(page: Page): string[] {
  const problems: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error' || message.type() === 'warning') {
      problems.push(`console.${message.type()}: ${message.text()}`);
    }
  });
  page.on('pageerror', (error) => problems.push(`pageerror: ${error.message}`));
  return problems;
}

async function loadHydratedPage(page: Page): Promise<string[]> {
  const problems = collectBrowserProblems(page);
  const response = await page.goto('/', { waitUntil: 'domcontentloaded' });
  expect(response?.status()).toBe(200);
  await expect(page.locator('.roll-button')).toContainText(
    /Create your hero before play|Inspect the runes|Runes already resolved/,
  );
  await ensureCampaignCreated(page);
  await page.waitForLoadState('networkidle');
  await page.waitForTimeout(100);
  return problems;
}

test('production SSR hydrates without changing stable shell content', async ({
  page,
  request,
}) => {
  const ssr = await request.get('/');
  expect(ssr.status()).toBe(200);
  const ssrBody = await ssr.text();
  const csp = ssr.headers()['content-security-policy'] ?? '';
  const headerNonce = csp.match(/'nonce-([^']+)'/)?.[1];
  const scriptNonces = [...ssrBody.matchAll(/<script[^>]+nonce="([^"]+)"/g)].map(
    (match) => match[1],
  );
  expect(headerNonce).toBeTruthy();
  expect(scriptNonces.length).toBeGreaterThan(0);
  expect(new Set(scriptNonces)).toEqual(new Set([headerNonce]));
  for (const marker of shellMarkers) {
    expect(ssrBody).toContain(marker);
  }
  expect(ssrBody).toContain('<html lang="en">');

  const problems = await loadHydratedPage(page);
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await expect(
    page.getByRole('heading', {
      level: 1,
      name: 'Your city. Your stories. A realm remade.',
    }),
  ).toBeVisible();
  await expect(page.locator('#getting-started')).toContainText(
    'Six saved steps to your first adventure',
  );
  await expect(
    page.getByRole('link', { name: 'Supported features', exact: true }),
  ).toHaveAttribute('href', '/guide');
  await page.locator('.native-theme-preview summary').click();
  await expect(page.locator('.selection-copy')).toContainText('Previewing: Rainbound Borough');
  await page.locator('label.theme-button').filter({ hasText: 'Emberline Archive' }).click();
  await expect(page.getByRole('radio', { name: /Emberline Archive/ })).toBeChecked();
  await expect(page.locator('.selection-copy')).toContainText('Previewing: Emberline Archive');
  expect(page.url()).not.toContain('theme=');
  expect(problems).toEqual([]);
});

test('provider-disabled gameplay commits once and reloads the stored result', async ({
  page,
}) => {
  const problems = await loadHydratedPage(page);
  await ensureHeroCreated(page);
  const action = page.locator('.roll-button');
  if (await action.isEnabled()) {
    await action.click();
  } else {
    await expect(action).toHaveText('Runes already resolved');
  }
  const readout = page.locator('.roll-readout');
  await expect(readout).toContainText('Saved roll');
  const committedResult = await readout.textContent();
  expect(committedResult).toMatch(/Revision \d+\./);

  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('.roll-button')).toContainText(/Inspect the runes|Runes already resolved/);
  await expect(readout).toHaveText(committedResult ?? '');
  await page.waitForTimeout(100);
  expect(problems).toEqual([]);
});

test('SSR remains useful when JavaScript and WASM are unavailable', async ({
  browser,
}, testInfo) => {
  const viewport = testInfo.project.name.includes('390x844')
    ? { width: 390, height: 844 }
    : { width: 1440, height: 900 };
  const context = await browser.newContext({ javaScriptEnabled: false, viewport });
  const page = await context.newPage();
  const response = await page.goto(baseURL, { waitUntil: 'domcontentloaded' });

  expect(response?.status()).toBe(200);
  await expect(
    page.getByRole('heading', {
      level: 1,
      name: 'Your city. Your stories. A realm remade.',
    }),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Inspect the runes' })).toBeDisabled();
  await expect(page.locator('.roll-readout')).toContainText('Loading the saved local campaign');

  // The same presentation-only theme form performs a native GET without WASM.
  // Hydration enhances its preview; authoritative creation still validates the
  // selected immutable pack on the server.
  await page.locator('.native-theme-preview summary').click();
  await page.locator('label.theme-button').filter({ hasText: 'Emberline Archive' }).click();
  await expect(page.getByRole('radio', { name: /Emberline Archive/ })).toBeChecked();
  await Promise.all([
    page.waitForNavigation({ waitUntil: 'domcontentloaded' }),
    page.getByRole('button', { name: 'Preview selected theme' }).click(),
  ]);
  expect(page.url()).toContain('theme=emberline-archive');
  await expect(page.locator('.selection-copy')).toContainText('Previewing: Emberline Archive');
  await context.close();
});

test('guide, privacy/reporting, and legal routes are useful SSR pages without WASM', async ({
  browser,
  request,
}, testInfo) => {
  const routes = [
    ['/guide', 'Set up safely, then follow the saved story'],
    ['/privacy-and-safety', 'Report a security, privacy, or safety issue'],
    ['/legal', 'System Reference Document 5.1'],
  ] as const;

  for (const [route, marker] of routes) {
    const response = await request.get(route);
    expect(response.status()).toBe(200);
    expect(await response.text()).toContain(marker);
    expect(response.headers()['cache-control']).toContain('no-store');
  }

  const viewport = testInfo.project.name.includes('390x844')
    ? { width: 390, height: 844 }
    : { width: 1440, height: 900 };
  const context = await browser.newContext({ javaScriptEnabled: false, viewport });
  const page = await context.newPage();
  for (const [route, marker] of routes) {
    const response = await page.goto(`${baseURL}${route}`, { waitUntil: 'domcontentloaded' });
    expect(response?.status()).toBe(200);
    await expect(page.locator('#main-content')).toContainText(marker);
    await expect(page.getByRole('link', { name: 'Skip to main content' })).toHaveAttribute(
      'href',
      '#main-content',
    );
  }
  await context.close();

  // Axe needs an active JavaScript event loop. Run it against the same SSR
  // routes in a separate context after the no-JavaScript assertions above.
  const accessibilityContext = await browser.newContext({ viewport });
  const accessibilityPage = await accessibilityContext.newPage();
  for (const [route] of routes) {
    const response = await accessibilityPage.goto(`${baseURL}${route}`, {
      waitUntil: 'networkidle',
    });
    expect(response?.status()).toBe(200);
    const accessibility = await new AxeBuilder({ page: accessibilityPage }).withTags([
      'wcag2a',
      'wcag2aa',
      'wcag21aa',
      'wcag22aa',
    ]).analyze();
    expect(accessibility.violations).toEqual([]);
  }
  await accessibilityContext.close();
});

test('WCAG automation, keyboard, reduced-motion, and responsive basics pass', async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  const problems = await loadHydratedPage(page);

  const accessibility = await new AxeBuilder({ page }).withTags([
    'wcag2a',
    'wcag2aa',
    'wcag21aa',
    'wcag22aa',
  ]).analyze();
  expect(accessibility.violations).toEqual([]);

  await page.keyboard.press('Tab');
  const keyboardFocus = await page.evaluate(() => {
    const active = document.activeElement;
    return active !== null && active !== document.body && active.matches(':focus-visible');
  });
  expect(keyboardFocus).toBe(true);

  const transitionDuration = await page.locator('.theme-button').first().evaluate((element) =>
    getComputedStyle(element).transitionDuration,
  );
  expect(transitionDuration).not.toContain('0.16s');

  const undersizedButtons = await page.locator('button').evaluateAll((buttons) =>
    buttons
      .map((button) => {
        const box = button.getBoundingClientRect();
        return { name: button.textContent?.trim() ?? '', width: box.width, height: box.height };
      })
      .filter(({ width, height }) => width < 24 || height < 24),
  );
  expect(undersizedButtons).toEqual([]);

  const overflowsViewport = await page.evaluate(
    () => document.documentElement.scrollWidth > window.innerWidth + 1,
  );
  expect(overflowsViewport).toBe(false);
  expect(problems).toEqual([]);
});
