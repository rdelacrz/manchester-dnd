# SQL-to-MongoDB Disposition

## Purpose

This is a completeness ledger for every final table in `.context/TABLE_SCHEMA.md`. It prevents useful behavior from disappearing merely because the new model consolidates normalized join tables.

Disposition terms:

- **Collection** — dedicated target collection.
- **Embed** — bounded subdocument/array inside the named aggregate.
- **Consolidate** — semantic record goes into a generic append-only collection.
- **Retire** — compatibility/storage concept intentionally removed.
- **External** — protected binary/body lives outside MongoDB; metadata remains in a collection.

This is not an ETL map. The database starts empty.

## Complete 70-table ledger

### Identity and authentication (1–4)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 1 | `account_sessions` | `account_sessions` | **Collection.** Preserve opaque bearer/CSRF digests, idle/absolute expiry, revocation, rotation. Add TTL cleanup but enforce expiry in application. |
| 2 | `accounts` | `accounts` | **Collection.** Add username/normalized username, encrypted email + keyed lookup HMAC, Argon2id PHC, role, login state, revisions. Retire `local_owner_id`. |
| 3 | `auth_throttle_buckets` | `auth_throttle_buckets` | **Collection.** Preserve digested rate-limit identity, bounded counters/windows, TTL. |
| 4 | `authentication_audits` | `audit_events` | **Consolidate** as category `authentication`; retain minimized metadata/outcome, never raw credentials/tokens/email. |

### Player character library (5–8)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 5 | `player_character_audits` | `audit_events` | **Consolidate** with scope `player_character`. |
| 6 | `player_character_command_receipts` | `command_receipts` | **Consolidate** with exact canonical fingerprint and scoped unique idempotency key. |
| 7 | `player_character_drafts` | `player_character_drafts` | **Collection.** Account-owned resumable draft with revision and TTL; discard import-era states. |
| 8 | `player_characters` | `player_characters` | **Collection.** Level-less reusable identity/build blueprint. Campaign stats stay out. |

### Campaign base (9–13)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 9 | `campaign_character_instances` | `campaign_character_instances` | **Collection.** Becomes the canonical campaign runtime character, merging useful hero runtime fields. |
| 10 | `campaign_content_pins` | `campaigns.rules_snapshot` | **Embed.** Bounded immutable pins always loaded with campaign; include versions/digests and sealed timestamp. |
| 11 | `campaign_invitations` | `campaign_invitations` | **Collection.** Invitations can grow historically and expire independently; store only token/code/email digests as applicable. |
| 12 | `campaign_memberships` | `campaigns.members[]` | **Embed.** Party roster is bounded and changed with campaign revision. Audit/history remains separate. |
| 13 | `campaign_sessions` | `campaigns` | **Collection.** Owner, lifecycle, bounded members/policies, rules snapshot, current pointers. Legacy import lifecycle fields retire. |

### Campaign lifecycle and play sessions (14–23)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 14 | `campaign_deletion_preparations` | `deletion_preparations` | **Collection.** Short-lived export/digest/confirmation with TTL and protected external body option. |
| 15 | `campaign_deletion_tombstones` | `deletion_tombstones` | **Consolidate** with entity kind `campaign`; survives parent deletion. |
| 16 | `campaign_lifecycle_audits` | `audit_events` | **Consolidate** category `campaign_lifecycle`. |
| 17 | `campaign_lifecycle_receipts` | `command_receipts` | **Consolidate** command family `campaign_lifecycle`. |
| 18 | `campaign_play_session_participants` | `play_sessions.participants[]` | **Embed.** Bounded roster, readiness, AI-substitution/handoff state; history in audits/turn events. |
| 19 | `campaign_play_sessions` | `play_sessions` | **Collection.** Lobby, membership snapshot, current mode/turn state, lifecycle. |
| 20 | `campaign_private_recaps` | `generated_presentations` | **Consolidate** presentation type `private_recap`; body may use protected external storage/encryption. |
| 21 | `campaign_turn_states` | `play_sessions.turn_state` | **Embed.** One current authoritative turn-control state; prior states are `turn_events`. |
| 22 | `lobby_command_receipts` | `command_receipts` | **Consolidate** command family `lobby`. |
| 23 | `turn_control_audits` | `audit_events` | **Consolidate** category `turn_control`; rich player-visible sequence remains in `turn_events`. |

### Generic game/encounter state (24–26)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 24 | `characters` | `campaign_character_instances`, `campaign_enemy_instances`, `encounters.combatants[]` | **Retire generic table.** Use typed player/enemy runtime aggregates and bounded encounter snapshots rather than mixed stringly typed state. |
| 25 | `command_receipts` | `command_receipts` | **Collection.** Expand to generic exact-replay schema; no weak global idempotency key. |
| 26 | `turn_audits` | `turn_events` + `audit_events` | **Split.** Rich deterministic mechanics/replay facts go to `turn_events`; minimized security/operator evidence to `audit_events`. |

### Encounter/hero mechanics (27–31)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 27 | `encounter_reward_claims` | `command_receipts` + terminal `turn_events`/encounter reward state | **Consolidate.** Exact-once claim is enforced by scoped unique receipt and encounter revision; result is in turn history. |
| 28 | `hero_audits` | `audit_events` | **Consolidate** scope `campaign_character_instance`. |
| 29 | `hero_characters` | `campaign_character_instances` | **Merge.** Progression, derived sheet, HP/resources become the campaign instance's progression/runtime fields. Eliminates duplicate bridge/runtime identities. |
| 30 | `hero_command_receipts` | `command_receipts` | **Consolidate** command family `character_progression`. |
| 31 | `hero_creation_drafts` | `player_character_drafts` or direct instance-creation receipt | **Merge/retire.** Character creation is account-library first; campaign instantiation is an idempotent command, not a second long-lived draft type. |

### Generation jobs/governance (32–35)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 32 | `generation_attempts` | `generation_jobs.attempts[]` | **Embed.** Attempts are hard-bounded by retry policy and always loaded with job. Store fingerprints/usage/failure, not secrets. |
| 33 | `generation_governance_diagnostics` | `audit_events` | **Consolidate** bounded operational diagnostic category with short TTL; metrics also exported to monitoring. |
| 34 | `generation_governance_receipts` | `generation_budget_reservations` | **Collection.** Preserve reservation/settlement/concurrency semantics and expiry. Command replay still uses `command_receipts`. |
| 35 | `generation_jobs` | `generation_jobs` | **Collection.** Durable queue, lease, retry, purpose/entity, request fingerprint, result pointers. |

### Generated text/image assets (36–41)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 36 | `generated_assets` | `generated_assets` | **Collection.** Metadata/provenance/authorization/digest only; binary body **External**. |
| 37 | `generated_text_presentation_receipts` | `command_receipts` | **Consolidate** command family `presentation`. |
| 38 | `generated_text_presentations` | `generated_presentations` | **Collection.** Versioned narration/dialogue/recap, selected state, audience, provenance. |
| 39 | `scene_image_artifacts` | `generated_assets` | **Merge.** Asset kind `scene_image`; variants can be bounded metadata subdocuments or separate asset versions. Bytes external. |
| 40 | `scene_image_quarantines` | `quarantined_assets` | **Collection.** Rejected temporary bytes/object references with reason and TTL; delete bytes first. |
| 41 | `typed_intent_command_receipts` | `command_receipts` | **Consolidate.** Pending/committed/failed state supported for two-phase typed proposals; bind proposal and base revisions. |

### Private inspiration campaign policy (42–48)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 42 | `campaign_inspiration_allowed_sensitivities` | `campaigns.safety.allowed_sensitivities[]` | **Embed.** Bounded policy set, revised with campaign safety aggregate. |
| 43 | `campaign_inspiration_excluded_participants` | `campaigns.safety.excluded_participant_ids[]` | **Embed.** Bounded policy set. |
| 44 | `campaign_inspiration_excluded_topics` | `campaigns.safety.excluded_topics[]` | **Embed.** Bounded policy set. |
| 45 | `campaign_inspiration_lines` | `campaigns.safety.lines[]` | **Embed.** Bounded safety policy. |
| 46 | `campaign_inspiration_settings` | `campaigns.safety` | **Embed.** Enabled/cursor/revision/retention policy with the campaign. |
| 47 | `campaign_inspiration_veils` | `campaigns.safety.veils[]` | **Embed.** Bounded safety policy. |
| 48 | `private_inspiration_command_receipts` | `command_receipts` | **Consolidate** command family `private_inspiration`. |

### Private inspiration consent and operations (49–60)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 49 | `private_inspiration_consent_grants` | `private_inspiration_consents` | **Collection.** Versioned campaign/source/participant consent and lifecycle. |
| 50 | `private_inspiration_consent_sensitivities` | `private_inspiration_consents.sensitivities[]` | **Embed.** Bounded set always interpreted with grant. |
| 51 | `private_inspiration_deletion_tombstones` | `deletion_tombstones` | **Consolidate** privacy entity kinds with shortened/minimized retention. |
| 52 | `private_inspiration_derived_work` | `private_inspiration_work` | **Collection.** Derived work state machine, cancellation/redaction/artifact links. |
| 53 | `private_inspiration_global_command_receipts` | `command_receipts` | **Consolidate** command family `private_inspiration_global`. |
| 54 | `private_inspiration_global_control` | `system_settings.private_inspiration` | **Embed singleton.** Kill switch/non-secret control state; every change audited. |
| 55 | `private_inspiration_participants` | `private_inspiration_participants` | **Collection.** Verified opaque participant identity/revocation evidence. |
| 56 | `private_inspiration_privacy_audits` | `audit_events` | **Consolidate** restricted category/retention with no raw source facts. |
| 57 | `private_inspiration_restricted_access_audits` | `audit_events` | **Consolidate** using restricted access/retention controls or external secure audit sink if required. |
| 58 | `private_inspiration_runtime_facts` | `private_inspiration_sources.runtime_projection.neutral_facts[]` | **Embed.** Hard-bounded minimized facts loaded with runtime projection. |
| 59 | `private_inspiration_runtime_prompts` | `private_inspiration_sources.runtime_projection` | **Embed.** Bounded sanitized selectable projection; raw source stays outside ordinary DB. |
| 60 | `private_inspiration_selection_audits` | `private_inspiration_selections` | **Collection.** Rich deterministic eligible-set/selection/cooldown proof; optional minimized mirror in `audit_events`. |

### Private inspiration source registry/use (61–67)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 61 | `private_inspiration_source_media` | `private_inspiration_sources.media[]` | **Embed metadata.** Hard-bounded media descriptors/digests; protected bytes **External**. |
| 62 | `private_inspiration_source_participants` | `private_inspiration_sources.participants[]` | **Embed.** Bounded participant-role references with source revision. |
| 63 | `private_inspiration_source_sensitivities` | `private_inspiration_sources.sensitivities[]` | **Embed.** Bounded reviewed tags. |
| 64 | `private_inspiration_source_themes` | `private_inspiration_sources.themes[]` | **Embed.** Bounded normalized themes; optional multikey index after query evidence. |
| 65 | `private_inspiration_source_usage` | `private_inspiration_selections` | **Merge.** Usage/cooldown lives in immutable selection entries; current cursor can be derived/materialized in campaign safety state. |
| 66 | `private_inspiration_sources` | `private_inspiration_sources` | **Collection.** Versioned review/digests/retention/runtime projection. |
| 67 | `private_inspiration_vetoes` | `private_inspiration_vetoes` | **Collection.** Independently mutable/queryable active vetoes. |

### BDE and recovery (68–70)

| # | PostgreSQL table | Target | Disposition / reason |
|---:|---|---|---|
| 68 | `custom_action_point_balances` | `campaign_character_instances.runtime.bde` | **Embed materialized balance.** Balance changes with character revision; ledger remains separate source of evidence. |
| 69 | `custom_action_point_ledger` | `bde_ledger` | **Collection.** Append-only signed deltas, balance-after, turn/receipt links, character-scoped idempotency. |
| 70 | `operator_recovery_status` | `system_settings.recovery` | **Embed singleton.** Non-secret backup/restore evidence; actual credentials/keys/manifests stored securely elsewhere. |

## Net-new target collections/fields required by the brief

The old SQL schema does not fully represent the requested product. Add:

| Target | Why net-new |
|---|---|
| `signup_access_tokens` | Operator-generated admission tokens are not normal account sessions. Store only digest/state/expiry/reservation. |
| `signup_sessions` | Explicit 24-hour account-setup session associated with one admission token. |
| `accounts.username`, `username_normalized`, `role` | Credential authentication by username and `admin|user` roles. |
| `accounts.email_ciphertext`, `email_lookup_hmac` | Recoverable encrypted email plus unique equality lookup. |
| `enemy_templates` | Admin-managed reusable enemy definitions with versioned full stat blocks. |
| `campaign_enemy_instances` | Snapshot and campaign-specific enemy/runtime state outside a single authored encounter. |
| `event_templates` | Admin-managed event definitions replacing/augmenting filesystem-only prompt registry. |
| `campaign_events` | Selected event snapshot, presentation/choice/resolution/cooldown linkage. |
| Generalized `encounters` | Current `characters`/authored encounter storage is too narrow for multi-combatant generated battles. |
| `quarantined_assets` | Dedicated safe cleanup boundary after merging scene-image tables. |
| `campaign_invitations` improvements | Account/email/join-code invitation flow with digests, TTL, state, and campaigns protected by auth. |

## Intentionally retired concepts

1. **PostgreSQL migrations and schema assumptions.** Replace with versioned Mongo collection/validator/index reconciliation.
2. **SQL foreign-key cascades.** Replace with explicit transaction/delete manifests and consistency checks.
3. **`local_owner_id` and legacy import provenance.** App is unlaunched; no compatibility need.
4. **`sqlite_row_id`/source-row fields and `legacy-import` CLI.** No data migration.
5. **One generic `characters` JSON envelope.** Typed runtime aggregates prevent mixing heroes, enemies, and encounter state.
6. **Separate hero and campaign-instance identities for the same runtime character.** Merge into one campaign character aggregate.
7. **Dozens of per-domain receipt/audit tables.** Generic collections with strict category/command discriminators and scoped unique indexes.
8. **Unbounded histories embedded in parent documents.** All such histories remain separate.
9. **Database byte storage for generated/private media.** Metadata only; protected external storage.

## Behavior preservation checklist

The consolidation is valid only if tests prove:

- all old ownership and two-account isolation guarantees;
- snapshots and content/prompt/rules digests remain immutable;
- optimistic revisions and exact idempotency remain strict;
- turn rolls/results are replayable;
- BDE, generation budgets, reward claims, and deletion commands are exactly once;
- session/invitation/token expiry is checked independent of TTL cleanup;
- active campaign/play/encounter deletion guards remain;
- private-inspiration consent, veto, cooldown, kill switch, redaction, and restricted audit behavior remain;
- generated artifacts cannot be accessed through guessed asset IDs;
- backup/restore reconstructs authoritative state and crypto dependencies.
