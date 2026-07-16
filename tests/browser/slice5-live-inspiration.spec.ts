import { execFileSync } from "node:child_process";
import { mkdirSync, unlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { expect, test, type Page } from "@playwright/test";

import { ensureHeroCreated } from "./support/hero-fixture";

const campaignId = "local-campaign";
const participantId = "participant:11111111111111111111111111111111";
const operatorId = "operator:22222222222222222222222222222222";
const rawCanaries = [
  "SYNTHETIC_TITLE_CANARY_7F2A91",
  "SYNTHETIC_RAW_SOURCE_CANARY_4D8C63",
];

function digest(character: string): string {
  return `sha256:${character.repeat(64)}`;
}

function databaseUrl(): string {
  const host = process.env.SLICE5_PGHOST ?? "127.0.0.1";
  const port = process.env.SLICE5_PGPORT ?? "5432";
  const user = process.env.SLICE5_PGUSER ?? "manchester_arcana";
  const password = process.env.SLICE5_PGPASSWORD ?? "manchester_arcana";
  const database =
    process.env.SLICE5_PGDATABASE ?? "manchester_arcana_slice5_browser";
  return `postgresql://${user}:${password}@${host}:${port}/${database}`;
}

function admin(command: Record<string, unknown>): unknown {
  const directory = "target/playwright/slice5-admin";
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
        IMAGE_LLM_BACKEND: "disabled",
        RNG_MASTER_KEY_FILE: ".runtime-private/playwright/slice5/rng-master.key",
        RUST_LOG: "off",
      },
    });
    const parsed = JSON.parse(output) as { ok: unknown };
    return parsed.ok;
  } finally {
    unlinkSync(path);
  }
}

function installConsentBoundary(): { sourceId: string; grantId: string } {
  const inventory = admin({ operation: "loaded_source_inventory" }) as Array<{
    source_id: string;
    source_digest: string;
    enabled: boolean;
  }>;
  expect(inventory).toHaveLength(1);
  expect(inventory[0].enabled).toBe(true);
  const sourceId = inventory[0].source_id;

  admin({
    operation: "configure_campaign",
    campaign_session_id: campaignId,
    idempotency_key: "slice5:settings:prepare",
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
    idempotency_key: "slice5:participant:verify",
    participant_id: participantId,
    method: "participant_signed_confirmation",
    evidence_digest: digest("2"),
    verifier_id: operatorId,
  });
  admin({
    operation: "register_loaded_source",
    campaign_session_id: campaignId,
    idempotency_key: "slice5:source:register",
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
    idempotency_key: "slice5:source:review",
    source_id: sourceId,
    source_version: 1,
    decision: "approved",
    reviewer_id: operatorId,
    review_evidence_digest: digest("4"),
  });
  admin({
    operation: "configure_campaign",
    campaign_session_id: campaignId,
    idempotency_key: "slice5:settings:enable",
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
  const grant = admin({
    operation: "grant_loaded_source_consent",
    campaign_session_id: campaignId,
    idempotency_key: "slice5:consent:grant",
    source_id: sourceId,
    source_version: 1,
    participant_id: participantId,
    expires_at_epoch: 4_102_444_800,
    reviewer_id: operatorId,
    participant_confirmation_digest: digest("5"),
    review_evidence_digest: digest("6"),
    artifact_policy: "delete_derived",
  }) as { grant_id: string };
  return { sourceId, grantId: grant.grant_id };
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

test("consented source reaches one safe-boundary narration and can be vetoed immediately", async ({
  page,
}) => {
  const browserBodies: string[] = [];
  page.on("response", async (response) => {
    const contentType = response.headers()["content-type"] ?? "";
    if (/text|json|javascript|css/.test(contentType)) {
      try {
        browserBodies.push(await response.text());
      } catch {
        // Navigations can retire a response before Playwright reads it.
      }
    }
  });

  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.locator(".roll-demo .save-status")).toContainText(
    /saved revision \d+/,
  );
  await ensureHeroCreated(page);
  const { sourceId, grantId } = installConsentBoundary();

  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator("#privacy").getByRole("status")).toContainText(
    "enabled behind current consent and safety gates",
  );
  await expect(
    page
      .locator("#privacy")
      .getByRole("button", { name: "Pause private generation" }),
  ).toBeEnabled();

  const runeAction = page.locator(".roll-button");
  if (await runeAction.isEnabled()) {
    await runeAction.click();
    await expect(page.locator(".roll-readout")).toContainText("Saved roll");
  }
  await expect(page.locator(".encounter-scene")).toBeVisible();

  for (let commandCount = 0; commandCount < 120; commandCount += 1) {
    const sceneText =
      (await page.locator(".encounter-scene").textContent()) ?? "";
    if (
      /Victory — transition saved|Defeat — recovery transition saved/.test(
        sceneText,
      )
    )
      break;

    const npcAdvance = page.locator(".npc-advance-action");
    if (await npcAdvance.isVisible()) {
      await npcAdvance.click();
      await expect(page.locator(".encounter-notice")).toContainText(
        "Saved deterministic policy step.",
      );
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
      await page
        .getByLabel("Describe another action")
        .fill("Attack the creature.");
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
        `supported direct action: ${labels.join(" | ")}`,
      ).toBeGreaterThanOrEqual(0);
      await actions.nth(preferred).click();
    }
    await expect.poll(() => encounterRevision(page)).not.toBe(before);
  }

  await expect(page.locator(".encounter-scene")).toContainText(
    /Victory — transition saved|Defeat — recovery transition saved/,
  );
  const history = page.locator(".narration-history");
  await expect(history).toContainText(
    "Consented, minimized, high-fiction-distance source used",
  );
  await expect(
    page.locator("#privacy").getByRole("button", { name: "Veto this source" }),
  ).toBeEnabled();

  const privateNarration =
    "Rain catches the lantern light as the recorded result settles into the scene. The committed mechanics remain exactly as resolved.";
  await expect(page.locator(".typed-gm-result")).toContainText(
    privateNarration,
  );
  await page
    .locator("#privacy")
    .getByRole("button", { name: "Veto this source" })
    .click();
  await expect(page.locator("#privacy").getByRole("status")).toContainText(
    "unrelated deterministic narration is shown",
  );
  await expect(page.locator(".typed-gm-result")).not.toContainText(
    privateNarration,
  );

  const participantExport = JSON.stringify(
    admin({
      operation: "participant_export",
      campaign_session_id: campaignId,
      requesting_participant_id: participantId,
    }),
  );
  for (const canary of rawCanaries)
    expect(participantExport).not.toContain(canary);

  const unavailableShare = await page.request.get(`/share/${sourceId}`);
  expect(unavailableShare.status()).toBe(404);
  expect(await unavailableShare.json()).toEqual({
    code: "public_sharing_unavailable",
  });

  admin({
    operation: "revoke_consent",
    campaign_session_id: campaignId,
    idempotency_key: "slice5:consent:revoke",
    grant_id: grantId,
    requester_participant_id: participantId,
    reason: "privacy_request",
  });

  const browserTranscript = browserBodies.join("\n");
  for (const canary of rawCanaries)
    expect(browserTranscript).not.toContain(canary);
  expect(browserTranscript).not.toContain(participantId);
  expect(browserTranscript).not.toContain(sourceId);
});
