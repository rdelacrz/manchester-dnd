import { execFileSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { ensureHeroCreated } from "./support/hero-fixture";

const campaignId = "local-campaign";
const participantId = "participant:11111111111111111111111111111111";
const operatorId = "operator:22222222222222222222222222222222";
const restartRequest =
  process.env.JOURNEY_RESTART_REQUEST ??
  "target/playwright/release-journey-restart.request";
const restartAck =
  process.env.JOURNEY_RESTART_ACK ??
  "target/playwright/release-journey-restart.ack";
const rawCanaries = [
  "SYNTHETIC_TITLE_CANARY_7F2A91",
  "SYNTHETIC_RAW_SOURCE_CANARY_4D8C63",
];

function digest(character: string): string {
  return `sha256:${character.repeat(64)}`;
}

function databaseUrl(): string {
  const host = process.env.JOURNEY_PGHOST ?? "127.0.0.1";
  const port = process.env.JOURNEY_PGPORT ?? "5432";
  const user = process.env.JOURNEY_PGUSER ?? "manchester_arcana";
  const password = process.env.JOURNEY_PGPASSWORD ?? "manchester_arcana";
  const database =
    process.env.JOURNEY_PGDATABASE ?? "manchester_arcana_release_journey";
  return `postgresql://${user}:${password}@${host}:${port}/${database}`;
}

function admin(command: Record<string, unknown>): unknown {
  const directory = "target/playwright/release-journey-admin";
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const path = join(directory, `${crypto.randomUUID()}.json`);
  writeFileSync(path, JSON.stringify(command), { mode: 0o600 });
  try {
    const output = execFileSync("target/release/inspiration-admin", [path], {
      encoding: "utf8",
      env: {
        ...process.env,
        APP_ENV_FILE: "/dev/null",
        APP_ACCESS_MODE: "local",
        DATABASE_URL: databaseUrl(),
        CONTENT_PACK_ROOT: "content/packs",
        EVENT_PROMPT_DIR: "tests/fixtures/private-inspiration",
        INSPIRATION_ENABLED: "true",
        TEXT_LLM_BACKEND: "fake",
        IMAGE_LLM_BACKEND: "fake",
        IMAGE_ARTIFACT_ROOT:
          ".runtime-private/playwright/release-journey/images",
        RNG_MASTER_KEY_FILE:
          ".runtime-private/playwright/release-journey/rng-master.key",
        RUST_LOG: "off",
      },
    });
    return (JSON.parse(output) as { ok: unknown }).ok;
  } finally {
    unlinkSync(path);
  }
}

function installConsentBoundary(): void {
  const inventory = admin({ operation: "loaded_source_inventory" }) as Array<{
    source_id: string;
    enabled: boolean;
  }>;
  expect(inventory).toHaveLength(1);
  expect(inventory[0].enabled).toBe(true);
  const sourceId = inventory[0].source_id;

  admin({
    operation: "configure_campaign",
    campaign_session_id: campaignId,
    idempotency_key: "journey:settings:prepare",
    expected_revision: 0,
    enabled: false,
    evidence_digest: digest("1"),
    reviewer_id: operatorId,
    tone: "gothic_adventure",
    allowed_sensitivity_codes: ["general"],
    line_codes: ["safety:graphic-gore"],
    veil_codes: ["safety:romance"],
    excluded_topic_codes: ["safety:children-in-danger"],
    excluded_participant_ids: [],
  });
  admin({
    operation: "verify_participant",
    campaign_session_id: campaignId,
    idempotency_key: "journey:participant:verify",
    participant_id: participantId,
    method: "participant_signed_confirmation",
    evidence_digest: digest("2"),
    verifier_id: operatorId,
  });
  admin({
    operation: "register_loaded_source",
    campaign_session_id: campaignId,
    idempotency_key: "journey:source:register",
    source_id: sourceId,
    source_version: 1,
    category_id: "category:journey",
    owner_participant_id: participantId,
    eligible_theme_pack_ids: ["dev.manchester-arcana.rainbound-borough"],
    provenance_digest: digest("3"),
    expires_at_epoch: 4_102_444_800,
  });
  admin({
    operation: "review_loaded_source",
    campaign_session_id: campaignId,
    idempotency_key: "journey:source:review",
    source_id: sourceId,
    source_version: 1,
    decision: "approved",
    reviewer_id: operatorId,
    review_evidence_digest: digest("4"),
  });
  admin({
    operation: "configure_campaign",
    campaign_session_id: campaignId,
    idempotency_key: "journey:settings:enable",
    expected_revision: 1,
    enabled: true,
    evidence_digest: digest("1"),
    reviewer_id: operatorId,
    tone: "gothic_adventure",
    allowed_sensitivity_codes: ["general"],
    line_codes: ["safety:graphic-gore"],
    veil_codes: ["safety:romance"],
    excluded_topic_codes: ["safety:children-in-danger"],
    excluded_participant_ids: [],
  });
  admin({
    operation: "grant_loaded_source_consent",
    campaign_session_id: campaignId,
    idempotency_key: "journey:consent:grant",
    source_id: sourceId,
    source_version: 1,
    participant_id: participantId,
    expires_at_epoch: 4_102_444_800,
    reviewer_id: operatorId,
    participant_confirmation_digest: digest("5"),
    review_evidence_digest: digest("6"),
    artifact_policy: "delete_derived",
  });
}

async function encounterRevision(page: Page): Promise<string> {
  return (
    (await page
      .locator(".encounter-meta div")
      .filter({ has: page.locator("dt", { hasText: "Encounter revision" }) })
      .locator("dd")
      .textContent()) ?? ""
  ).trim();
}

async function playToVictory(page: Page): Promise<void> {
  for (let commandCount = 0; commandCount < 160; commandCount += 1) {
    const sceneText =
      (await page.locator(".encounter-scene").textContent()) ?? "";
    if (sceneText.includes("Victory — transition saved")) return;
    expect(sceneText).not.toContain("Defeat — recovery transition saved");

    const npcAdvance = page.locator(".npc-advance-action");
    if (await npcAdvance.isVisible()) {
      const before = await encounterRevision(page);
      await npcAdvance.click();
      await expect(page.locator(".encounter-notice")).toContainText(
        "Saved deterministic policy step.",
      );
      await expect.poll(() => encounterRevision(page)).not.toBe(before);
      continue;
    }

    const actions = page.locator(".encounter-actions .encounter-action");
    await expect(actions.first()).toBeEnabled();
    const labels = (await actions.allTextContents()).map((label) =>
      label.trim(),
    );
    const before = await encounterRevision(page);
    const attack = labels.findIndex((label) => label.startsWith("Attack "));
    if (attack >= 0) {
      await page.getByLabel("Describe another action").fill("Attack the creature.");
      const response = page.waitForResponse((candidate) =>
        candidate.url().includes("/api/submit_typed_player_intent"),
      );
      await page
        .getByRole("button", { name: "Interpret against legal actions" })
        .click();
      expect((await response).ok()).toBe(true);
    } else {
      const preferred =
        labels.findIndex((label) => label.includes("Release the sluice")) >= 0
          ? labels.findIndex((label) => label.includes("Release the sluice"))
          : labels.findIndex((label) => label.startsWith("Move to ")) >= 0
            ? labels.findIndex((label) => label.startsWith("Move to "))
            : labels.findIndex((label) => label.includes("death save")) >= 0
              ? labels.findIndex((label) => label.includes("death save"))
              : labels.findIndex((label) => label.includes("initiative")) >= 0
                ? labels.findIndex((label) => label.includes("initiative"))
                : labels.findIndex((label) =>
                    label.includes("End the current turn"),
                  );
      expect(
        preferred,
        `supported journey action: ${labels.join(" | ")}`,
      ).toBeGreaterThanOrEqual(0);
      await actions.nth(preferred).click();
    }
    await expect.poll(() => encounterRevision(page)).not.toBe(before);
  }
  throw new Error("the deterministic journey did not reach victory in 160 commands");
}

function normalizedExport(body: string): unknown {
  const parsed = JSON.parse(body) as Record<string, unknown>;
  delete parsed.exported_at;
  return parsed;
}

async function requestSupervisorRestart(page: Page): Promise<void> {
  if (existsSync(restartAck)) unlinkSync(restartAck);
  mkdirSync(join(restartRequest, ".."), { recursive: true });
  writeFileSync(restartRequest, "restart\n", { mode: 0o600 });

  await expect
    .poll(() => (existsSync(restartAck) ? readFileSync(restartAck, "utf8").trim() : ""), {
      timeout: 30_000,
    })
    .toBe("1");
  await expect
    .poll(
      async () => {
        try {
          return (await page.request.get("/health/ready")).status();
        } catch {
          return 0;
        }
      },
      { timeout: 30_000 },
    )
    .toBe(204);
}

test("a player creates, socializes, explores, wins, exports, restarts, and resumes", async ({
  page,
}) => {
  const browserBodies: string[] = [];
  page.on("response", async (response) => {
    const contentType = response.headers()["content-type"] ?? "";
    if (/text|json|javascript|css/.test(contentType)) {
      try {
        browserBodies.push(await response.text());
      } catch {
        // A restart can retire a navigation response before it is read.
      }
    }
  });

  const response = await page.goto("/", { waitUntil: "domcontentloaded" });
  expect(response?.status()).toBe(200);
  await ensureHeroCreated(page);
  await expect(page.locator(".created-hero")).toContainText("Level 1");
  const lifecycle = page.locator("#campaigns");
  const startPlay = lifecycle.getByRole("button", {
    name: "Start play session",
  });
  if (await startPlay.isVisible()) {
    await startPlay.click();
    await expect(
      lifecycle.getByRole("button", { name: "End play session" }),
    ).toBeVisible();
  }

  const social = page.locator(".social-panel");
  await social
    .getByRole("button", { name: "Ask Elin about the sealed rain gate" })
    .click();
  await expect(social.locator(".social-notice")).toContainText(
    "Saved social roll",
  );
  await expect(social).toContainText("mapped DC 15");
  await expect(social).toContainText("Soot tide: 1/4");

  installConsentBoundary();
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator("#privacy").getByRole("status")).toContainText(
    "enabled behind current consent and safety gates",
  );
  await expect(social.locator(".social-notice")).toContainText(
    "Saved social roll",
  );

  await page.locator(".roll-button").click();
  await expect(page.locator(".roll-readout")).toContainText("Saved roll");
  await expect(page.locator(".encounter-scene")).toBeVisible();

  const begin = page.getByRole("button", {
    name: "Roll initiative and begin",
  });
  if (await begin.isVisible()) {
    await begin.click();
    await expect(page.locator(".encounter-notice")).toContainText("Saved.");
  }

  await playToVictory(page);
  await expect(page.locator(".encounter-scene")).toContainText(
    "Victory — transition saved",
  );
  const imagePanel = page.locator("#scene-image");
  await imagePanel.getByRole("button", { name: "Request scene image" }).click();
  await expect(imagePanel.locator(".scene-image-state")).toContainText(
    "Verified image ready",
  );
  const imageSource = await imagePanel.locator("img").getAttribute("src");
  expect(imageSource).toMatch(/^\/api\/local\/images\/.+\/web$/);
  const imageResponse = await page.request.get(imageSource ?? "");
  expect(imageResponse.status()).toBe(200);
  const imageBody = await imageResponse.body();
  expect(imageBody.byteLength).toBeGreaterThan(8);
  expect(imageBody.subarray(0, 8)).toEqual(
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  );
  await expect(page.locator(".narration-history")).toContainText(
    "Consented, minimized, high-fiction-distance source used",
  );

  await page.getByRole("button", { name: "Claim completed encounter XP" }).click();
  await expect(page.locator(".hero-save-state")).toContainText(
    "Victory reward saved. Level 2 is now available.",
  );
  await page.getByRole("button", { name: "Apply validated level-up" }).click();
  await expect(page.locator(".hero-save-state")).toContainText(
    "Level 2 choices and derived sheet saved atomically.",
  );
  await expect(page.locator(".created-hero")).toContainText("Level 2");

  await lifecycle.getByRole("button", { name: "Load history" }).click();
  await expect(lifecycle.getByRole("status")).toContainText(
    "History rendered from saved audits only",
  );
  const historyTurns = await lifecycle.locator(".campaign-history details").count();
  expect(historyTurns).toBeGreaterThan(5);
  await lifecycle
    .getByRole("button", { name: "Build/update private recap" })
    .click();
  await expect(lifecycle.getByRole("status")).toContainText(
    "Private recap saved with its committed-audit provenance.",
  );
  const recap = (await lifecycle.locator(".private-recap pre").textContent()) ?? "";
  expect(recap).toContain("Turn 1");
  await lifecycle.getByRole("button", { name: "Canonical export" }).click();
  await expect(lifecycle.locator(".private-export-field textarea")).toHaveValue(
    /^\{/,
  );

  await lifecycle.getByRole("button", { name: "End play session" }).click();
  await expect(
    lifecycle.getByRole("button", { name: "Start play session" }),
  ).toBeVisible();
  await lifecycle.getByRole("button", { name: "Canonical export" }).click();
  await expect(lifecycle.getByRole("status")).toContainText(
    "Canonical private export is ready",
  );
  const durableExport = await lifecycle
    .locator(".private-export-field textarea")
    .inputValue();
  expect(durableExport).toContain('"private_recaps":[');

  await requestSupervisorRestart(page);
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator(".created-hero")).toContainText("Level 2");
  await expect(page.locator(".social-panel .social-notice")).toContainText(
    "Saved social roll",
  );
  await expect(page.locator(".encounter-scene")).toContainText(
    "Victory — transition saved",
  );
  await expect(page.locator("#scene-image img")).toHaveAttribute(
    "src",
    imageSource ?? "",
  );
  expect((await page.request.get(imageSource ?? "")).status()).toBe(200);

  const resumedLifecycle = page.locator("#campaigns");
  await expect(
    resumedLifecycle.getByRole("button", { name: "Start play session" }),
  ).toBeVisible();
  await resumedLifecycle
    .getByRole("button", { name: "Load saved private recap" })
    .click();
  await expect(resumedLifecycle.locator(".private-recap pre")).toHaveText(recap);
  await resumedLifecycle.getByRole("button", { name: "Load history" }).click();
  await expect(resumedLifecycle.locator(".campaign-history details")).toHaveCount(
    historyTurns,
  );
  await resumedLifecycle
    .getByRole("button", { name: "Canonical export" })
    .click();
  await expect(resumedLifecycle.getByRole("status")).toContainText(
    "Canonical private export is ready",
  );
  const resumedExport = await resumedLifecycle
    .locator(".private-export-field textarea")
    .inputValue();
  expect(normalizedExport(resumedExport)).toEqual(normalizedExport(durableExport));

  const transcript = browserBodies.join("\n");
  for (const canary of rawCanaries) expect(transcript).not.toContain(canary);
  expect(transcript).not.toContain(participantId);
});
