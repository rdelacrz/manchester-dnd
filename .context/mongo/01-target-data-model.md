# Target MongoDB Data Model

## Modeling rules

1. **One aggregate, one authoritative revision.** Mutable root documents have `schema_version`, `revision`, `created_at`, and `updated_at`.
2. **Server-derived partitions.** Account-owned documents carry `owner_account_id`; campaign documents carry `campaign_id`; browser-facing reads and writes always include the authenticated account's ownership/membership predicate.
3. **Bounded embedding only.** Embed data only when a documented limit exists and the data is normally loaded/mutated with its parent.
4. **Separate unbounded history.** Turns, audits, receipts, invitations, generation work, asset versions, BDE entries, and privacy history use separate collections.
5. **Immutable snapshots.** Runtime instances carry source revision/digest/payload snapshots. Never follow a mutable template to resolve an already-started campaign.
6. **No database secrets.** Documents may hold ciphertext, digests, key IDs, and non-secret provider/model identifiers. Keys, raw tokens, and provider credentials stay outside MongoDB.
7. **Strict validation twice.** MongoDB `$jsonSchema` catches shape/type/range errors; Rust `serde(deny_unknown_fields)` and domain `validate()` methods enforce richer invariants.
8. **No implicit cascade.** Every destructive workflow has an explicit child-collection manifest and transaction/cleanup policy.

## Collection catalog

### Identity and access

| Collection | Purpose | Important fields / embedded data | Required indexes | Retention |
|---|---|---|---|---|
| `accounts` | Human account and login verifier | `_id`, `role`, `username`, `username_normalized`, `email_ciphertext`, `email_key_id`, `email_lookup_hmac`, `password_phc`, `password_changed_at`, `login_enabled`, timestamps | unique `username_normalized`; unique `email_lookup_hmac`; `{role, created_at}` | Account lifetime; security audit survives separately |
| `signup_access_tokens` | CLI-generated, one-use admission token | `_id`, `token_digest`, `state`, `allowed_role`, `issued_by`, `expires_at`, reservation/redemption metadata | unique `token_digest`; `{state, expires_at}`; TTL `purge_at` | Short; consumed/revoked record retained briefly, then TTL |
| `signup_sessions` | 24-hour account-setup session created from an access token | `_id`, `token_digest`, `csrf_digest`, `access_token_id`, `state`, `expires_at`, `purge_at` | unique token and CSRF digests; partial unique active `access_token_id`; TTL `purge_at` | 24 hours active plus short audit grace |
| `account_sessions` | Normal authenticated browser session | `_id`, `account_id`, bearer/CSRF digests, idle/absolute expiry, revocation and rotation metadata | unique bearer and CSRF digests; `{account_id, revoked_at, created_at}`; TTL `purge_at` | Policy-defined; app checks expiry before TTL cleanup |
| `auth_throttle_buckets` | Login/sign-up rate-limit state without raw email/IP | `_id` or `{key_digest, action_kind}`, window, count, blocked time, `purge_at` | unique `{key_digest, action_kind}`; TTL `purge_at` | Short operational TTL |
| `campaign_invitations` | Optional invitation to join a campaign | campaign/inviter IDs, invitee-email or join-code digest, state, expiry, accepted account | unique active code/email digest as applicable; `{campaign_id, state, expires_at}`; TTL `purge_at` | Active until accepted/revoked/expired; then short retention |

### Player library, campaign definitions, and runtime instances

| Collection | Purpose | Important fields / embedded data | Required indexes | Retention |
|---|---|---|---|---|
| `player_character_drafts` | Resumable account-owned creation workflow | owner, step, revision, partial choices, reviewed/committed state, expiry | `{owner_account_id, updated_at:-1}`; TTL `purge_at` | Expiring |
| `player_characters` | Level-less reusable character record | owner, name, presentation, rules/build blueprint, image, revision | unique `{owner_account_id, display_name_normalized}`; `{owner_account_id, updated_at:-1}` | Owner lifetime; deletion blocked while active instances reference it |
| `campaigns` | Campaign aggregate, owner, bounded roster, policy, sealed rules snapshot | owner, title, lifecycle, `members[]`, `rules_snapshot`, safety settings, current pointers | `{owner_account_id, updated_at:-1}`; multikey `{members.account_id, members.state, updated_at:-1}`; optional unique owner/title | Campaign lifetime |
| `campaign_character_instances` | Authoritative campaign-specific player character | campaign/account/source IDs, immutable source snapshot, progression, runtime state, BDE balance, revision | partial unique active `{campaign_id, account_id}`; `{account_id, source_player_character_id, campaign_id}`; `{campaign_id, state}` | Campaign lifetime; source library deletion restricted while active |
| `enemy_templates` | Admin-managed reusable enemy definition | logical ID/revision, public and hidden descriptions, complete mechanics, image, state | unique `{logical_id, revision}`; partial unique current `{logical_id}`; `{state, updated_at:-1}` | Version history retained; updates create a new revision |
| `campaign_enemy_instances` | Campaign-scoped enemy snapshot and mutable state | owner/campaign, source snapshot, level/CR, HP/resources/conditions, reveal state, revision | `{campaign_id, state, updated_at:-1}`; `{source.logical_id, source.revision}` | Campaign lifetime |
| `event_templates` | Admin-managed possible event definition | logical ID/revision, eligibility, weight/cooldown, sensitivity, bounded prompt/effect intents | unique `{logical_id, revision}`; partial unique current `{logical_id}`; `{state, eligibility.mode}` | Version history retained |
| `campaign_events` | Instantiated event with immutable source snapshot and resolution | campaign/play-session/turn IDs, snapshot, choices, status, outcome, revision | `{campaign_id, created_at:-1}`; `{play_session_id, turn_sequence}`; `{campaign_id, status}` | Campaign lifetime or bounded recap policy |

### Active play, battles, replay, and economy

| Collection | Purpose | Important fields / embedded data | Required indexes | Retention |
|---|---|---|---|---|
| `play_sessions` | Lobby plus current exploration/battle control state | campaign/GM, state, `participants[]`, start policy, membership snapshot, `mode`, `turn_state`, revision | partial unique open `{campaign_id}` for `waiting|active`; `{participants.account_id, state}`; `{campaign_id, opened_at:-1}` | Campaign lifetime; closed sessions immutable except redaction |
| `encounters` | Authoritative battle state | campaign/play-session, combatant snapshots/references, initiative, round/actor, action economy, effects, status, revision | partial unique active `{play_session_id}`; `{campaign_id, created_at:-1}` | Campaign lifetime; finalized encounters immutable |
| `turn_events` | Append-only mechanics and player/GM presentation chronology | scope, sequence, mode/phase, actor, intent, random facts, before/after revisions, result, safe display references | unique `{play_session_id, sequence}`; `{campaign_id, created_at:-1}`; `{correlation_id}` | Campaign lifetime; optional archive tier later |
| `command_receipts` | Exact idempotent replay across command types | actor/scope/command/key, canonical request fingerprint, result revisions/response, state, retention | unique `{scope_kind, scope_id, idempotency_key}`; `{actor_account_id, created_at:-1}`; TTL `purge_at` when allowed | Command-specific; destructive receipts outlive deleted parent |
| `audit_events` | Minimized immutable security/domain/operations evidence | category/action/outcome, actor, scope, revision, correlation ID, bounded metadata, retention | `{scope_kind, scope_id, created_at:-1}`; `{actor_account_id, created_at:-1}`; `{category, created_at:-1}`; TTL where allowed | Policy-specific; no narration/private prompt bodies |
| `bde_ledger` | Append-only grant/spend/refund ledger | instance/account/campaign/play-session/turn, signed delta, reason, balance-after, receipt/event link | unique `{campaign_character_instance_id, idempotency_key}`; `{play_session_id, created_at:-1}` | Campaign lifetime; no weak global key |

### AI generation and artifacts

| Collection | Purpose | Important fields / embedded data | Required indexes | Retention |
|---|---|---|---|---|
| `generation_jobs` | Durable queue/lease for text and image generation | campaign/purpose/entity, state, priority, request fingerprint, bounded `attempts[]`, lease/retry fields | unique `{campaign_id, purpose, idempotency_key}`; `{state, available_at, priority:-1}`; `{lease_expires_at}` | Settled job metadata retained by policy |
| `generation_budget_reservations` | Atomic budget/concurrency reservation and settlement | job/scope/dimension, reserved/spent values, state, expiry | unique `{job_id, dimension}`; `{scope_kind, scope_id, state, expires_at}`; TTL `purge_at` | Short after settlement |
| `generated_presentations` | Versioned GM narration, event prose, and private recap bodies | campaign/origin event, type, version, selected/privacy state, body or protected-body reference, provider evidence | unique `{campaign_id, origin_event_id, version}`; partial unique selected `{campaign_id, origin_event_id}`; `{campaign_id, created_at:-1}` | Selected content campaign lifetime; superseded bodies may expire |
| `generated_assets` | Metadata for character/campaign/scene images and other media | owner/campaign/entity, object key, digest, media dimensions/type, provider/model, moderation/state | `{owner_account_id, entity_kind, entity_id}`; `{campaign_id, created_at:-1}`; unique object digest/key as appropriate | Entity/campaign lifetime; bytes external |
| `quarantined_assets` | Metadata for rejected unsafe/invalid external bytes | job/attempt/object key, reason code, expiry | `{purge_at}` TTL; `{job_id}` | Short; external bytes deleted before metadata |

### Private inspiration, deletion, and operations

| Collection | Purpose | Important fields / embedded data | Required indexes | Retention |
|---|---|---|---|---|
| `private_inspiration_participants` | Verified participant identity for consent workflows | opaque participant ID, verification/revocation evidence | unique participant ID; state/time index | Policy-defined minimized metadata |
| `private_inspiration_sources` | Versioned reviewed source metadata and sanitized runtime projection | logical ID/revision/digests, owner participant, review state, embedded bounded media/participants/sensitivities/themes/runtime facts | unique `{logical_id, revision}`; candidate indexes on review/theme/expiry | Protected source body remains outside ordinary DB |
| `private_inspiration_consents` | Campaign/source/participant consent grant | audience/media/transformation/artifact policy, embedded sensitivities, state, expiry | unique versioned grant key; campaign/source/state indexes | Consent policy |
| `private_inspiration_vetoes` | Active participant/campaign-owner veto | campaign, actor, scope (campaign/category/source), evidence | `{campaign_id, state, scope_kind, category_id, source_id}` | Retained as privacy evidence |
| `private_inspiration_selections` | Deterministic selection proof plus cooldown usage | campaign/turn/cursor, eligible-set digest, selected source, next eligible turn, result | unique `{campaign_id, idempotency_key}`; `{campaign_id, source_id, turn_number:-1}` | Campaign/privacy policy |
| `private_inspiration_work` | Derived-work state machine | selection/job/artifact references, state, cancellation/redaction, privacy | unique selection/work IDs; `{campaign_id, state}` | Privacy policy |
| `deletion_preparations` | Short-lived canonical export awaiting destructive confirmation | owner/scope/deletion ID, revisions, digest, protected export reference/body, expiry | unique deletion ID; TTL `purge_at` | About one hour |
| `deletion_tombstones` | Minimal post-delete replay/ID-reuse marker | entity kind/ID, owner digest/ID where permitted, deletion revision/digest, `purge_at` | unique `{entity_kind, entity_id, deletion_id}`; TTL `purge_at` | 30–35 days or policy |
| `system_settings` | Singleton non-secret operational controls | schema version, private-inspiration kill switch, recovery status, policy identifiers | `_id` only (`system:settings`) | Permanent; revisions audited |

## Representative document shapes

The examples are contracts, not seed data. Timestamps are BSON dates in implementation.

### `accounts`

```json
{
  "_id": "account:<uuid>",
  "schema_version": 1,
  "revision": 1,
  "role": "user",
  "username": "VisibleName",
  "username_normalized": "visiblename",
  "email_ciphertext": {
    "algorithm": "xchacha20poly1305",
    "key_id": "email-key:2026-01",
    "nonce_b64": "<random nonce>",
    "ciphertext_b64": "<ciphertext and tag>"
  },
  "email_lookup_hmac": "hmac-sha256:<hex>",
  "password_phc": "$argon2id$v=19$...",
  "login_enabled": true,
  "password_changed_at": "<BSON date>",
  "created_at": "<BSON date>",
  "updated_at": "<BSON date>"
}
```

Rules:

- `role` is `admin|user`; web sign-up can create only `user`.
- Admin creation/promotion is CLI-only and audited.
- A randomized nonce is mandatory; `_id` and `key_id` are authenticated additional data.
- `email_lookup_hmac` is computed from the canonical normalized email with a separate secret lookup key.
- Password PHC carries algorithm, parameters, salt, and digest; no separate password salt field.

### `player_characters`

```json
{
  "_id": "character:<uuid>",
  "schema_version": 1,
  "revision": 4,
  "owner_account_id": "account:<uuid>",
  "display_name": "Mara Venn",
  "display_name_normalized": "mara venn",
  "presentation": {
    "pronouns": ["she", "her"],
    "description": "<bounded player-authored description>",
    "portrait_asset_id": "asset:<uuid>"
  },
  "ruleset_id": "srd-5.2.1",
  "build_blueprint": {
    "species_id": "species:human",
    "background_id": "background:soldier",
    "starting_class_id": "class:fighter",
    "planned_subclass_id": "subclass:champion",
    "ability_generation": { "method": "standard_array", "assignments": {} },
    "proficiency_choices": [],
    "language_choices": [],
    "feat_choices": [],
    "starting_equipment_choices": [],
    "spell_choices": [],
    "future_choices": []
  },
  "created_at": "<BSON date>",
  "updated_at": "<BSON date>"
}
```

Forbidden fields include `level`, `experience_points`, HP, hit dice, spell slots, conditions, inventory mutations, money, BDE, and campaign ID. A blueprint may express intended class/subclass choices, but active features are derived only in a campaign instance at its campaign-specific level.

### `campaigns`

```json
{
  "_id": "campaign:<uuid>",
  "schema_version": 1,
  "revision": 12,
  "owner_account_id": "account:<uuid>",
  "title": "Rain over Ancoats",
  "lifecycle": { "state": "open", "archived_at": null },
  "members": [
    {
      "account_id": "account:<uuid>",
      "role": "game_master",
      "state": "active",
      "joined_at": "<BSON date>",
      "left_at": null
    }
  ],
  "rules_snapshot": {
    "ruleset_id": "srd-5.2.1",
    "ruleset_digest": "sha256:<hex>",
    "content_packs": [{ "id": "pack:<id>", "version": 1, "digest": "sha256:<hex>" }],
    "progression_policy_id": "progression:xp:v1",
    "safety_policy_id": "safety:private:v1",
    "gm_prompt_id": "typed-game-master:v1",
    "gm_prompt_digest": "sha256:<hex>",
    "sealed_at": "<BSON date>"
  },
  "game_policy": {
    "party_size_max": 6,
    "leveling": "experience_points",
    "lethality": "story_recovery",
    "start_policy": "wait_for_all",
    "bde": { "starting": 3, "maximum": 10, "custom_action_cost": 1 }
  },
  "safety": {
    "lines": [],
    "veils": [],
    "excluded_topics": [],
    "allowed_sensitivities": [],
    "excluded_participant_ids": [],
    "inspiration_enabled": false,
    "revision": 0
  },
  "current_play_session_id": null,
  "created_at": "<BSON date>",
  "updated_at": "<BSON date>"
}
```

`members` and safety arrays are embedded only after enforcing explicit maximum lengths. Historical invitations and audits are not embedded.

### `campaign_character_instances`

```json
{
  "_id": "campaign-character:<uuid>",
  "schema_version": 1,
  "revision": 33,
  "campaign_id": "campaign:<uuid>",
  "account_id": "account:<uuid>",
  "source_player_character_id": "character:<uuid>",
  "state": "active",
  "source_snapshot": {
    "source_revision": 4,
    "source_schema_version": 1,
    "source_digest": "sha256:<hex>",
    "captured_at": "<BSON date>",
    "display_name": "Mara Venn",
    "presentation": {},
    "build_blueprint": {}
  },
  "progression": {
    "level": 5,
    "experience_points": 7000,
    "milestone_count": 0,
    "level_choices": [],
    "class_progression": [{ "class_id": "class:fighter", "levels": 5, "subclass_id": "subclass:champion" }]
  },
  "derived_sheet": {
    "derivation_id": "rules:srd-5.2.1:<digest>",
    "proficiency_bonus": 3,
    "ability_scores": {},
    "ability_modifiers": {},
    "saving_throws": [],
    "skills": [],
    "passive_values": {},
    "armor_class": 18,
    "speed": {},
    "maximum_hit_points": 44,
    "features": [],
    "attacks": [],
    "spellcasting": null
  },
  "runtime": {
    "current_hit_points": 31,
    "temporary_hit_points": 0,
    "death_saves": { "successes": 0, "failures": 0 },
    "hit_dice": [],
    "resource_pools": [],
    "conditions": [],
    "exhaustion": 0,
    "inventory": [],
    "equipped": {},
    "attunements": [],
    "currency": {},
    "spell_state": null,
    "concentration": null,
    "bde": { "balance": 3, "lifetime_earned": 3, "lifetime_spent": 0 }
  },
  "created_at": "<BSON date>",
  "updated_at": "<BSON date>",
  "retired_at": null
}
```

Derived maxima and features are recomputed from pinned rules and explicit choices; clients cannot write them directly. Runtime counters are checked against derived limits.

### `enemy_templates` and campaign snapshot

An enemy template must support more than a name/level:

```json
{
  "_id": "enemy-template:<uuid>:v3",
  "logical_id": "enemy-template:<uuid>",
  "revision": 3,
  "schema_version": 1,
  "state": "current",
  "created_by_account_id": "account:<admin>",
  "name": "Canal Wight",
  "public_description": "...",
  "hidden_gm_notes_ciphertext": null,
  "ruleset_id": "srd-5.2.1",
  "stat_block": {
    "size": "medium",
    "creature_type": "undead",
    "alignment": "neutral_evil",
    "challenge_rating": "3",
    "experience_reward": 700,
    "proficiency_bonus": 2,
    "armor_class": 14,
    "hit_points": { "average": 45, "formula": "6d8+18" },
    "speeds": {},
    "ability_scores": {},
    "saving_throws": [],
    "skills": [],
    "vulnerabilities": [],
    "resistances": [],
    "damage_immunities": [],
    "condition_immunities": [],
    "senses": [],
    "languages": [],
    "traits": [],
    "actions": [],
    "bonus_actions": [],
    "reactions": [],
    "legendary_actions": [],
    "lair_actions": [],
    "spellcasting": null,
    "loot_table_id": null
  },
  "generation": { "behavior_tags": [], "image_prompt_template_id": null },
  "created_at": "<BSON date>",
  "updated_at": "<BSON date>"
}
```

A `campaign_enemy_instances` document copies the template's source metadata and complete stat block into `source_snapshot`, then adds mutable HP, resources, conditions, position, visibility, and revision. Editing the template creates a new version; it never changes the instance snapshot.

### `play_sessions`

```json
{
  "_id": "play-session:<uuid>",
  "schema_version": 1,
  "revision": 21,
  "campaign_id": "campaign:<uuid>",
  "gm_account_id": "account:<uuid>",
  "state": "active",
  "start_policy": "start_with_ai_substitutes",
  "membership_snapshot": { "campaign_revision": 12, "member_ids": [] },
  "participants": [
    {
      "account_id": "account:<uuid>",
      "campaign_character_instance_id": "campaign-character:<uuid>",
      "state": "human_active",
      "ready_at": "<BSON date>",
      "handoff_revision": 0
    }
  ],
  "mode": "exploration",
  "turn_state": {
    "phase": "gm_generation",
    "sequence": 42,
    "round": null,
    "active_account_id": null,
    "active_character_instance_id": null,
    "active_encounter_id": null,
    "legal_action_set_digest": null,
    "based_on_event_sequence": 41
  },
  "opened_at": "<BSON date>",
  "closed_at": null,
  "close_reason": null,
  "updated_at": "<BSON date>"
}
```

Participant arrays are party-size bounded. Dialogue and event history are references into `turn_events`/`generated_presentations`, never an embedded growing array.

### `encounters`

`encounters` embeds the bounded combat roster and current battle state because the roster, initiative, action economy, and effects are usually read together. Each hero combatant references its campaign character instance and carries an immutable encounter-start rules snapshot; each enemy combatant references its campaign enemy instance and snapshot.

Minimum durable state:

```text
campaign_id, play_session_id, revision, status
rules/content snapshot identities
combatants[] with source IDs, public/hidden snapshots, HP/temp HP, death saves,
  position, resources, conditions/effects, concentration, reaction state
initiative {order[], rolls[], tie_breaks[]}
round, current_actor_index, current_actor_id
action_economy {action, bonus_action, reaction, object_interaction, movement}
objectives[], map/zone state, pending reaction/decision
reward eligibility and exact-once claim state
created_at, started_at, ended_at
```

Set hard limits for combatants, effects per combatant, map objects, and pending reactions. A future mass battle uses a different aggregate rather than increasing these limits without bound.

## Index rules and caveats

### Partial uniqueness

Use named partial unique indexes for:

- one active campaign character per `{campaign_id, account_id}`;
- one open play session per campaign;
- one active encounter per play session;
- one current enemy/event template revision per logical ID;
- one selected presentation version per origin event.

The service query must include the partial filter predicate for reliable index use.

### TTL

Use a dedicated BSON date `purge_at` with `expireAfterSeconds: 0`. Do not combine TTL with compound identity lookup. TTL is cleanup only; application queries must reject expired documents even if the TTL monitor has not removed them.

### Encrypted fields

Do not index `email_ciphertext` or encrypted private text. Index only a separately keyed digest designed for equality lookup. A partial-filter expression must not reference a Queryable Encryption field, and Queryable Encryption cannot guarantee plaintext uniqueness.

### Multikey indexes

Only bounded arrays receive multikey indexes (`campaigns.members`, `play_sessions.participants`, selected source tags). Do not create compound indexes that require more than one array field. Verify representative queries with `explain("executionStats")` before declaring an index complete.

## Validation strategy

Create every collection explicitly with:

```text
validationLevel: strict
validationAction: error
validator: { $jsonSchema: ... }
```

Validators must cover:

- required `_id`, `schema_version`, revision, timestamp, partition, and state fields;
- `bsonType` for every field;
- enums and ID/digest patterns;
- min/max numeric ranges;
- bounded string byte/character policy enforced again in Rust;
- bounded array lengths;
- `additionalProperties: false` where practical, explicitly allowing `_id`;
- state-shape rules expressible with `$and`/`$or`/`$expr` outside `$jsonSchema`.

Validators cannot replace:

- ownership/membership authorization;
- cross-collection existence and same-campaign checks;
- canonical request fingerprint verification;
- derived-sheet validation;
- legal action derivation;
- BDE/HP/resource transaction logic;
- snapshot digest verification;
- deletion cascade manifests.

## Cross-document invariants enforced by service transactions

1. A campaign owner is an active `game_master` member in the same campaign.
2. A campaign character's account is an active campaign member and owns the source library character.
3. A play-session participant references an active campaign character for the same account and campaign.
4. At most one play session is waiting/active per campaign and one encounter is active per play session.
5. Encounter combatants reference instances from the same campaign and preserve source-snapshot digests.
6. Turn sequence and aggregate revisions advance exactly once per committed command.
7. BDE balance cannot become negative; state mutation, ledger entry, turn event, audit, and receipt commit together.
8. Generated presentation/asset publication references a settled job/attempt and an unchanged origin revision.
9. Deleting a player character is rejected while any active campaign instance references it.
10. Deleting a campaign is rejected while an open play session or active encounter exists.

## Document-size budgets

Do not merely target MongoDB's 16 MiB hard limit. Adopt lower application limits:

- campaign aggregate: target < 256 KiB;
- play session: target < 256 KiB;
- encounter: target < 1 MiB and hard bounded combat roster;
- character/enemy/event snapshot: target < 256 KiB;
- receipt response: <= 64 KiB;
- audit metadata: <= 16 KiB;
- text presentation body: policy limit <= 256 KiB;
- generated binary media: never BSON; external storage or GridFS only if object storage is unavailable.

Add BSON encoded-size tests for maximum-valid fixtures.
