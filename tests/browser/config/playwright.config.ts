import { defineConfig, devices } from '@playwright/test';
import path from 'node:path';

const repositoryRoot = path.resolve(__dirname, '../../..');
const address = process.env.PLAYWRIGHT_ADDRESS ?? '127.0.0.1:6789';
const baseURL = process.env.PLAYWRIGHT_BASE_URL ?? `http://${address}`;
const desktopViewport = { width: 1440, height: 900 };
const mobileViewport = { width: 390, height: 844 };
const webkitLaunchOptions = process.env.PLAYWRIGHT_WEBKIT_EXECUTABLE_PATH
  ? { launchOptions: { executablePath: process.env.PLAYWRIGHT_WEBKIT_EXECUTABLE_PATH } }
  : {};

export default defineConfig({
  testDir: path.join(repositoryRoot, 'tests/browser'),
  testIgnore: [
    'release-journey.spec.ts',
    'slice3-narration.spec.ts',
    'slice5-live-inspiration.spec.ts',
    'slice6-scene-images.spec.ts',
  ],
  outputDir: path.join(repositoryRoot, 'target/playwright/test-results'),
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  reporter: process.env.CI
    ? [
        ['line'],
        [
          'html',
          {
            outputFolder: path.join(repositoryRoot, 'target/playwright/report'),
            open: 'never',
          },
        ],
      ]
    : [
        ['list'],
        [
          'html',
          {
            outputFolder: path.join(repositoryRoot, 'target/playwright/report'),
            open: 'never',
          },
        ],
      ],
  use: {
    baseURL,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  expect: {
    timeout: 15_000,
  },
  timeout: 45_000,
  projects: [
    {
      name: 'chromium-desktop-1440x900',
      use: { browserName: 'chromium', viewport: desktopViewport },
    },
    {
      name: 'firefox-desktop-1440x900',
      use: { browserName: 'firefox', viewport: desktopViewport },
    },
    {
      name: 'webkit-desktop-1440x900',
      use: { browserName: 'webkit', viewport: desktopViewport, ...webkitLaunchOptions },
    },
    {
      name: 'chromium-android-emulation-390x844',
      use: { ...devices['Pixel 7'], viewport: mobileViewport },
    },
    {
      name: 'firefox-responsive-390x844',
      use: { browserName: 'firefox', viewport: mobileViewport, hasTouch: true },
    },
    {
      name: 'webkit-ios-emulation-390x844',
      use: { ...devices['iPhone 14'], viewport: mobileViewport, ...webkitLaunchOptions },
    },
  ],
  webServer: {
    command:
      process.env.PLAYWRIGHT_WEB_SERVER_COMMAND ??
      path.join(repositoryRoot, 'target/release/manchester-dnd-web'),
    cwd: repositoryRoot,
    url: `${baseURL}/health/ready`,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
    stdout: 'pipe',
    stderr: 'pipe',
    env: {
      ...process.env,
      APP_ENV_FILE: '/dev/null',
      APP_ACCESS_MODE: 'local',
      LEPTOS_SITE_ADDR: address,
      LEPTOS_SITE_ROOT:
        process.env.LEPTOS_SITE_ROOT ?? path.join(repositoryRoot, 'target/site'),
      DATABASE_URL:
        process.env.DATABASE_URL ??
        'postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/manchester_arcana',
      EVENT_PROMPT_DIR:
        process.env.EVENT_PROMPT_DIR ?? path.join(repositoryRoot, 'prompts/events/private'),
      TEXT_LLM_BACKEND: 'disabled',
      IMAGE_LLM_BACKEND: 'disabled',
      RUST_LOG: process.env.RUST_LOG ?? 'manchester_dnd=info,tower_http=info',
    },
  },
});
