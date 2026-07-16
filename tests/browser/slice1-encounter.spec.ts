import { expect, test } from '@playwright/test';

import { ensureCampaignCreated, ensureHeroCreated } from './support/hero-fixture';

async function loadEncounter(page: import('@playwright/test').Page) {
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await ensureHeroCreated(page);
  const runeAction = page.locator('.roll-button');
  if (await runeAction.isEnabled()) {
    await runeAction.click();
    await expect(page.locator('.roll-readout')).toContainText('Saved roll');
  }
  await expect(page.locator('.encounter-scene')).toBeVisible();
}

async function encounterSnapshot(page: import('@playwright/test').Page) {
  return page.locator('.encounter-scene').evaluate((scene) => ({
    status: scene.querySelector('.encounter-meta')?.textContent ?? '',
    combatants: scene.querySelector('.combatants')?.textContent ?? '',
    revision: [...scene.querySelectorAll('.encounter-meta div')]
      .find((item) => item.querySelector('dt')?.textContent === 'Encounter revision')
      ?.querySelector('dd')?.textContent ?? '',
  }));
}

async function replaceWithFreshCampaign(page: import('@playwright/test').Page) {
  const panel = page.locator('#campaigns');
  const endPlay = panel.getByRole('button', { name: 'End play session' });
  if (await endPlay.isVisible()) {
    await endPlay.click();
    await expect(panel.getByRole('button', { name: 'Start play session' })).toBeVisible();
  }

  await panel.getByRole('button', { name: 'Archive' }).click();
  await expect(panel.getByRole('button', { name: 'Restore archive' })).toBeVisible();
  await panel.getByRole('button', { name: 'Prepare permanent delete' }).click();
  await expect(panel.locator('.delete-confirmation')).toBeVisible();
  await panel.getByRole('button', { name: 'Confirm permanent delete' }).click();
  const create = panel.getByRole('button', { name: 'Create local campaign' });
  await expect(create).toBeVisible();
  await create.click();
  await expect(panel.locator('.campaign-library-card')).toBeVisible();
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('#campaigns .campaign-library-card')).toBeVisible();
}

async function verifyServerRejectsForgedTurn(
  page: import('@playwright/test').Page,
  legalRequest: import('@playwright/test').Request,
  verifyMalformed: boolean,
): Promise<boolean> {
  const origin = new URL(page.url()).origin;
  const encodedLegalCommand = legalRequest.postData();
  expect(encodedLegalCommand).toBeTruthy();

  if (verifyMalformed) {
    const forgedMechanics = new URLSearchParams(encodedLegalCommand ?? '');
    forgedMechanics.set(
      'command[command][idempotency_key]',
      crypto.randomUUID(),
    );
    forgedMechanics.set('command[command][intent][roll]', '20');
    forgedMechanics.set(
      'command[command][actor_id]',
      'manchester-arcana-content:v1:hero:canal-warden',
    );
    const forgedResponse = await page.request.post(legalRequest.url(), {
      headers: {
        accept: 'application/json',
        'content-type': 'application/x-www-form-urlencoded',
        origin,
      },
      data: forgedMechanics.toString(),
    });
    expect(forgedResponse.status()).toBe(400);
    expect(await forgedResponse.json()).toEqual({
      code: 'invalid_server_input',
    });
  }

  const loadUrl = await page.evaluate(() =>
    performance
      .getEntriesByType('resource')
      .map((entry) => entry.name)
      .find((url) => url.includes('/api/load_local_campaign')),
  );
  expect(loadUrl).toBeTruthy();
  const loadedResponse = await page.request.post(loadUrl ?? '', {
    headers: {
      accept: 'application/json',
      'content-type': 'application/x-www-form-urlencoded',
      origin,
    },
    data: '',
  });
  expect(loadedResponse.status()).toBe(200);
  const loaded = await loadedResponse.json();
  expect(loaded.status).toBe('ready');
  const encounter = loaded.payload.encounter;
  const state = encounter.state;
  expect(state.current_actor_id).toBeTruthy();

  const heroTurn = state.current_actor_id === state.hero.id;
  if (heroTurn) return false;
  const outOfTurn = new URLSearchParams({
    'command[schema_version]': '1',
    'command[campaign_session_id]': loaded.payload.campaign_session_id,
    'command[expected_campaign_revision]': String(encounter.campaign_revision),
    'command[command][schema_version]': '1',
    'command[command][encounter_id]': state.encounter_id,
    'command[command][expected_revision]': String(state.revision),
    'command[command][idempotency_key]': crypto.randomUUID(),
    // End turn is mechanically legal for the active creature. The player endpoint must still
    // reject it because the browser is never the creature's controller.
    'command[command][intent][type]': 'end_turn',
  });
  const outOfTurnResponse = await page.request.post(legalRequest.url(), {
    headers: {
      accept: 'application/json',
      'content-type': 'application/x-www-form-urlencoded',
      origin,
    },
    data: outOfTurn.toString(),
  });
  expect(outOfTurnResponse.status()).toBe(200);
  const rejected = await outOfTurnResponse.json();
  expect(rejected.status).toBe('rejected');
  expect(rejected.payload.code).toBe('not_player_turn');
  return true;
}

test('the deterministic encounter plays, saves, reloads, and explains its rolls', async ({
  page,
}) => {
  await loadEncounter(page);
  if ((await page.locator('.encounter-actions .encounter-action').count()) > 0) {
    const beforeFallback = await encounterSnapshot(page);
    await page.getByLabel('Describe another action').fill('Strike the creature.');
    await page.getByRole('button', { name: 'Interpret against legal actions' }).click();
    await expect(page.locator('.typed-gm-result.degraded')).toContainText(
      'Deterministic degraded mode',
    );
    await page.locator('.generation-evidence summary').first().click();
    await expect(page.locator('.generation-evidence').first()).toContainText('authored_fallback');
    await expect(page.locator('.generation-evidence').first()).toContainText('unavailable');
    await expect.poll(() => encounterSnapshot(page)).toEqual(beforeFallback);
  }
  let reloadedMidEncounter = false;
  let malformedChecked = false;
  let authorityChecked = false;
  let playerEndpointRequest: import('@playwright/test').Request | undefined;

  for (let commandCount = 0; commandCount < 120; commandCount += 1) {
    const scene = page.locator('.encounter-scene');
    const text = await scene.textContent();
    if (text?.includes('Victory — transition saved') || text?.includes('Defeat — recovery transition saved')) {
      break;
    }

    const npcAdvance = page.locator('.npc-advance-action');
    if (await npcAdvance.isVisible()) {
      await expect(page.locator('.encounter-actions .encounter-action')).toHaveCount(0);
      await expect(page.locator('.npc-turn-control')).toContainText(
        'No creature action, target, destination, or roll is selected by this browser.',
      );
      await expect(
        page.getByRole('button', { name: 'Interpret against legal actions' }),
      ).toBeDisabled();

      if (playerEndpointRequest && !authorityChecked) {
        const beforeForgery = await encounterSnapshot(page);
        authorityChecked = await verifyServerRejectsForgedTurn(
          page,
          playerEndpointRequest,
          !malformedChecked,
        );
        malformedChecked = true;
        await page.reload({ waitUntil: 'domcontentloaded' });
        await expect(page.locator('.encounter-scene')).toBeVisible();
        await expect.poll(() => encounterSnapshot(page)).toEqual(beforeForgery);
        reloadedMidEncounter = true;
      }

      const npcRequestPromise = page.waitForRequest(
        (request) =>
          request.method() === 'POST' && request.url().includes('/api/advance_npc_turn'),
      );
      await npcAdvance.click();
      const npcRequest = await npcRequestPromise;
      const npcBody = npcRequest.postData() ?? '';
      expect(npcBody).not.toMatch(
        /actor_id|action_id|attack_id|target_id|destination_feet|intent|roll|damage/,
      );
      await expect(page.locator('.encounter-notice')).toContainText(
        'Saved deterministic policy step.',
      );
      continue;
    }

    const actions = page.locator('.encounter-actions .encounter-action');
    await expect(actions.first()).toBeEnabled();
    const labels = await actions.allTextContents();
    const preferred =
      labels.findIndex((label) => label.trim().startsWith('Attack ')) >= 0
        ? labels.findIndex((label) => label.trim().startsWith('Attack '))
        : labels.findIndex((label) => label.includes('Release the sluice')) >= 0
          ? labels.findIndex((label) => label.includes('Release the sluice'))
          : labels.findIndex((label) => label.startsWith('Move to ')) >= 0
            ? labels.findIndex((label) => label.startsWith('Move to '))
            : labels.findIndex((label) => label.includes('death save')) >= 0
              ? labels.findIndex((label) => label.includes('death save'))
              : labels.findIndex((label) => label.includes('initiative')) >= 0
                ? labels.findIndex((label) => label.includes('initiative'))
                : labels.findIndex((label) => label.includes('End the current turn'));
    expect(preferred, `legal actions: ${labels.join(' | ')}`).toBeGreaterThanOrEqual(0);

    const legalRequestPromise = !playerEndpointRequest
      ? page.waitForRequest(
          (request) =>
            request.method() === 'POST' &&
            request.url().includes('/api/submit_encounter_action'),
        )
      : undefined;
    await actions.nth(preferred).click();
    const legalRequest = await legalRequestPromise;
    await expect(page.locator('.encounter-notice')).toContainText('Saved.');

    if (legalRequest) {
      playerEndpointRequest = legalRequest;
      const beforeForgery = await encounterSnapshot(page);
      authorityChecked = await verifyServerRejectsForgedTurn(
        page,
        legalRequest,
        !malformedChecked,
      );
      malformedChecked = true;
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect(page.locator('.encounter-scene')).toBeVisible();
      await expect.poll(() => encounterSnapshot(page)).toEqual(beforeForgery);
      reloadedMidEncounter = true;
    }

    if (!reloadedMidEncounter && commandCount >= 2) {
      const before = await encounterSnapshot(page);
      await page.reload({ waitUntil: 'domcontentloaded' });
      await expect(page.locator('.encounter-scene')).toBeVisible();
      await expect.poll(() => encounterSnapshot(page)).toEqual(before);
      reloadedMidEncounter = true;
    }
  }

  const finalText = await page.locator('.encounter-scene').textContent();
  expect(finalText).toContain('Victory — transition saved');

  await page.getByRole('button', { name: 'Claim completed encounter XP' }).click();
  await expect(page.locator('.hero-save-state')).toContainText(
    'Victory reward saved. Level 2 is now available.',
  );
  await page.getByRole('button', { name: 'Apply validated level-up' }).click();
  await expect(page.locator('.hero-save-state')).toContainText(
    'Level 2 choices and derived sheet saved atomically.',
  );
  await expect(page.locator('.created-hero')).toContainText('Level 2');
  await page.reload({ waitUntil: 'domcontentloaded' });
  await expect(page.locator('.created-hero')).toContainText('Level 2');

  const explanation = page.locator('.roll-explanation');
  if (await explanation.isVisible()) {
    await explanation.locator('summary').click();
    await expect(explanation).toContainText(/RNG chacha20-v1 cursor \d+→\d+/);
    await expect(explanation).toContainText('seed reference');
    await expect(explanation).toContainText('Source srd-5.1-cc');
  }
});

test('the authored safety path reaches deterministic defeat without reward', async ({ page }) => {
  test.setTimeout(120_000);
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await ensureCampaignCreated(page);
  await replaceWithFreshCampaign(page);
  await ensureHeroCreated(page);

  const runeAction = page.locator('.roll-button');
  await runeAction.click();
  await expect(page.locator('.roll-readout')).toContainText('Saved roll');
  await expect(page.locator('.encounter-scene')).toBeVisible();

  for (let commandCount = 0; commandCount < 160; commandCount += 1) {
    const text = await page.locator('.encounter-scene').textContent();
    if (text?.includes('Defeat — recovery transition saved')) break;

    const npcAdvance = page.locator('.npc-advance-action');
    if (await npcAdvance.isVisible()) {
      await npcAdvance.click();
      await expect(page.locator('.encounter-notice')).toContainText(
        'Saved deterministic policy step.',
      );
      continue;
    }

    const actions = page.locator('.encounter-actions .encounter-action');
    await expect(actions.first()).toBeEnabled();
    const labels = await actions.allTextContents();
    const preferred = labels.findIndex((label) => label.includes('initiative')) >= 0
      ? labels.findIndex((label) => label.includes('initiative'))
      : labels.findIndex((label) => label.includes('End the current turn')) >= 0
        ? labels.findIndex((label) => label.includes('End the current turn'))
        : labels.findIndex((label) => label.includes('death save'));
    expect(preferred, `legal defeat-path actions: ${labels.join(' | ')}`).toBeGreaterThanOrEqual(0);
    await actions.nth(preferred).click();
    await expect(page.locator('.encounter-notice')).toContainText('Saved.');
  }

  await expect(page.locator('.encounter-scene')).toContainText(
    'Defeat — recovery transition saved',
  );
  await expect(page.locator('.level-preview')).toContainText(
    'Complete the authored encounter to earn trusted XP.',
  );

  // Leave an empty active campaign so the independent Slice 2 test still
  // exercises every resumable creation step from a fresh durable state.
  await replaceWithFreshCampaign(page);
  await expect(page.getByRole('button', { name: 'Begin guided creation' })).toBeVisible();
});
