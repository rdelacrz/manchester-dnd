import { defineConfig } from "@playwright/test";
import path from "node:path";

const repositoryRoot = path.resolve(__dirname, "../../..");
const address = process.env.PLAYWRIGHT_ADDRESS ?? "127.0.0.1:6797";
const baseURL = process.env.PLAYWRIGHT_BASE_URL ?? `http://${address}`;

export default defineConfig({
  testDir: path.join(repositoryRoot, "tests/browser"),
  testMatch: "release-journey.spec.ts",
  outputDir: path.join(
    repositoryRoot,
    "target/playwright/release-journey-test-results",
  ),
  workers: 1,
  retries: 0,
  reporter: [
    ["list"],
    [
      "html",
      {
        outputFolder: path.join(
          repositoryRoot,
          "target/playwright/release-journey-report",
        ),
        open: "never",
      },
    ],
  ],
  use: {
    baseURL,
    browserName: "chromium",
    viewport: { width: 1440, height: 900 },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  expect: { timeout: 20_000 },
  timeout: 300_000,
  webServer: {
    command: path.join(
      repositoryRoot,
      "tests/browser/support/run-release-journey-server.sh",
    ),
    cwd: repositoryRoot,
    url: `${baseURL}/health/ready`,
    reuseExistingServer: false,
    timeout: 60_000,
    stdout: "pipe",
    stderr: "pipe",
    env: {
      ...process.env,
      APP_ENV_FILE: "/dev/null",
      APP_ACCESS_MODE: "local",
      LEPTOS_SITE_ADDR: address,
      LEPTOS_SITE_ROOT: process.env.LEPTOS_SITE_ROOT ?? "target/site",
      EVENT_PROMPT_DIR: "tests/fixtures/private-inspiration",
      INSPIRATION_ENABLED: "true",
      TEXT_LLM_BACKEND: "fake",
      IMAGE_LLM_BACKEND: "fake",
      IMAGE_ARTIFACT_ROOT:
        ".runtime-private/playwright/release-journey/images",
      RNG_MASTER_KEY_FILE:
        ".runtime-private/playwright/release-journey/rng-master.key",
      RUST_LOG: process.env.RUST_LOG ?? "manchester_dnd=info",
    },
  },
});
