import { expect, test } from "@playwright/test";

import { ensureHeroCreated } from "./support/hero-fixture";

async function mechanicsSnapshot(page: import("@playwright/test").Page) {
  return page.locator(".encounter-scene").evaluate((scene) => ({
    metadata: scene.querySelector(".encounter-meta")?.textContent ?? "",
    combatants: scene.querySelector(".combatants")?.textContent ?? "",
    actions: scene.querySelector(".encounter-actions")?.textContent ?? "",
    narration: scene.querySelector(".encounter-narration")?.textContent ?? "",
  }));
}

test("durable optional image appears through an authorized verified variant and replaces once", async ({
  page,
}) => {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(page.locator(".roll-demo .save-status")).toContainText(
    /saved revision \d+/,
  );
  await ensureHeroCreated(page);

  const runeAction = page.locator(".roll-button");
  if (await runeAction.isEnabled()) {
    await runeAction.click();
    await expect(page.locator(".roll-readout")).toContainText("Saved roll");
  }
  await expect(page.locator(".encounter-scene")).toBeVisible();
  const begin = page.getByRole("button", {
    name: "Roll initiative and begin",
  });
  if (await begin.isVisible()) {
    await begin.click();
    await expect(page.locator(".encounter-notice")).toContainText("Saved.");
  }
  const mechanics = await mechanicsSnapshot(page);

  const imagePanel = page.locator("#scene-image");
  await expect(imagePanel).toContainText(
    "excludes private inspiration, player text, names, likenesses",
  );
  const request = imagePanel.getByRole("button", {
    name: "Request scene image",
  });
  await expect(request).toBeEnabled();

  let discarded = false;
  await page.route("**/api/request_scene_image*", async (route) => {
    if (discarded) {
      await route.continue();
      return;
    }
    discarded = true;
    const response = await route.fetch();
    expect(response.ok()).toBeTruthy();
    await route.abort("failed");
  });
  await request.click();
  await expect(imagePanel.locator(".scene-image-notice")).toContainText(
    "response was interrupted",
  );
  await page.unroute("**/api/request_scene_image*");
  await imagePanel
    .getByRole("button", { name: "Retry exact image request" })
    .click();

  await expect(imagePanel.locator(".scene-image-state")).toContainText(
    "Verified image ready",
  );
  const firstImage = imagePanel.locator("img");
  await expect(firstImage).toBeVisible();
  const firstAlt = await firstImage.getAttribute("alt");
  expect(firstAlt).toBeTruthy();
  const firstSource = await firstImage.getAttribute("src");
  expect(firstSource).toMatch(/^\/api\/local\/images\/.+\/web$/);
  expect(
    await firstImage.evaluate(
      (image: HTMLImageElement) => image.complete && image.naturalWidth > 0,
    ),
  ).toBe(true);

  const delivered = await page.request.get(firstSource!);
  expect(delivered.status()).toBe(200);
  expect(delivered.headers()["content-type"]).toBe("image/png");
  expect(delivered.headers()["cache-control"]).toContain("no-store");
  expect((await delivered.body()).subarray(0, 8)).toEqual(
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  );
  const original = await page.request.get(firstSource!.replace(/\/web$/, "/original"));
  expect(original.status()).toBe(404);
  expect(await mechanicsSnapshot(page)).toEqual(mechanics);

  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator("#scene-image img")).toBeVisible();
  await expect(
    page.locator("#scene-image").getByRole("button", {
      name: "Request the one replacement",
    }),
  ).toBeEnabled();
  await page
    .locator("#scene-image")
    .getByRole("button", { name: "Request the one replacement" })
    .click();
  await expect
    .poll(() => page.locator("#scene-image img").getAttribute("src"))
    .not.toBe(firstSource);
  const replacementSource = await page.locator("#scene-image img").getAttribute("src");
  expect(replacementSource).not.toBe(firstSource);
  await expect(page.locator("#scene-image .scene-image-budget")).toContainText(
    "2/2 for this scene",
  );
  await expect(
    page.locator("#scene-image").getByRole("button", {
      name: "Request scene image",
    }),
  ).toBeDisabled();
  expect(await mechanicsSnapshot(page)).toEqual(mechanics);
});
