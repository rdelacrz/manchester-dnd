# PostgreSQL Table Schema and Access Patterns

This document describes the **final public application schema after migrations `0001`–`0031`**. It was verified by applying every migration, in order, to an isolated PostgreSQL 16 instance and reading the resulting PostgreSQL catalogs.

- **Application tables:** 70
- **Columns:** 696
- **Constraints:** 702
- **Indexes (including constraint-backed indexes):** 190
- **Excluded:** SQLx’s internal `_sqlx_migrations` bookkeeping table and role-only SQL in `scripts/postgres-roles.sql`.
- **Type spelling:** Types are PostgreSQL catalog output (`timestamp with time zone`, `jsonb`, `bigint`, and so on), not Rust aliases.

## How to read the access notes

- **Aggregate/current-state tables** are mutable and normally protected by row locks or optimistic revisions.
- **Audit/ledger/receipt tables** are append-only unless a stated retention cleanup deletes them. Receipts provide exact idempotent replay; audits/ledgers provide history.
- **Queue/workflow tables** are mutable state machines. Worker concurrency uses leases and, where noted, `FOR UPDATE SKIP LOCKED`.
- **Ownership scope matters:** `owner_account_id`/`account_id` are hosted account boundaries; `owner_key` remains the local/legacy lifecycle boundary; `campaign_session_id` is the campaign partition.
- **Schema-ready** means the migration exists but current production Rust SQL does not yet read or write the table.

## Character table distinctions

| Table | Meaning |
|---|---|
| `player_characters` | Account-owned, reusable, **level-less** character library record. |
| `campaign_character_instances` | Bridge/snapshot binding a library character to one campaign and one runtime hero. |
| `hero_characters` | Campaign-specific runtime hero with level, XP, HP, resources, and derived state in JSON. |
| `characters` | Older generic campaign-character aggregate used by the atomic turn repository and legacy import. |

## Important transitional and integrity caveats

1. **Hosted ownership is dual-mode.** `owner_account_id` is nullable for legacy/local campaigns; `owner_key` remains active. Hosted authorization must use active membership/account predicates and must not infer access from a nullable owner column.
2. **Migration 0030 is only partly wired.** Existing lifecycle code uses `campaign_play_sessions`, but `campaign_play_session_participants`, `campaign_turn_states`, `turn_control_audits`, and `lobby_command_receipts` have no production repository persistence yet.
3. **Migration 0031 is repository-only.** `ActionPointRepository` is marked dead code and has no application/turn-commit caller. Its helper transaction is not the same transaction as the authoritative mechanics commit.
4. **Custom-action referential gaps are deliberate current facts.** `custom_action_point_balances` has no FKs; ledger `play_session_id` has no FK; ledger `turn_revision` and `amount` have no database range checks; idempotency uniqueness is global on `(idempotency_key, reason)`.
5. **Custom-action ledger rows block parent deletion.** The ledger FKs to `accounts`, `campaign_sessions`, and `hero_characters` use PostgreSQL's default `NO ACTION`. Once a ledger row exists, the current hard-delete paths for that account, campaign, or runtime hero fail unless the ledger row is removed first. This has been reproduced against the fully migrated PostgreSQL 16 schema; it overrides older “all campaign children cascade” assumptions. The balance table does not cause this blocker because it has no FKs.
6. **Custom-action replay identity is weak.** An `(idempotency_key, reason)` conflict is accepted without comparing account, campaign, character, play session, turn revision, or amount. The code then returns the current balance for the newly supplied entity key, so changed-input replay is not safely fingerprinted. Balance upserts also retain the original `play_session_id` because the conflict update changes only balance and timestamp.
7. **Lifecycle delete replay survives deletion.** `campaign_lifecycle_receipts` deliberately has no campaign FK and is retained for 30 days; do not describe it as campaign-cascaded.
8. **Membership and runtime-character integrity is partly application-enforced.** No composite FK ties `campaign_character_instances` to membership, member removal does not retire an instance, and the database does not require the active GM to equal the campaign owner account.
9. **Some character-library UI writes bypass the application workflow.** The current browser component calls repository-level display-name update and delete methods directly. Those paths therefore bypass the application service's audit and idempotency-receipt writes. Even within the application service, mutation, audit, and receipt calls use separate transactions and are not crash-atomic.
10. **Player-character draft retention differs from policy comments.** Maintenance deletes rows as soon as `expires_at` passes; there is no separate 30-day retention deadline like the one on `hero_creation_drafts`.
11. **Invitation expiry is logical, not cleanup-driven.** Redemption predicates reject expired invitations, but no dedicated hard-delete cleanup currently removes them.
12. **Participant deletion is privacy minimization, not full relational erasure.** Protected bodies are removed and grants/work are revoked or redacted, while opaque participant/source/audit metadata remains for compliance/history.
13. **JSON validation varies by generation.** Newer JSONB columns often require an object and cap byte size; early aggregate JSONB and player-character choices rely more heavily on Rust validation. Read the exact CHECK constraints per table.
14. **Concurrency uses row locks, not advisory locks.** The code uses `FOR UPDATE`, `FOR SHARE`, `SKIP LOCKED`, optimistic revisions, and a serializable legacy import; no PostgreSQL advisory-lock calls were found.

## Quick access-pattern index

| Table | Domain | Dominant pattern |
|---|---|---|
| [`account_sessions`](#account-sessions) | Authentication and identity | The hot path looks up an unrevoked, unexpired row by unique `token_digest`, then advances `last_seen_at` and the bounded idle deadline. Sessio… |
| [`accounts`](#accounts) | Authentication and identity | Login loads by normalized email; registration and password paths write identity state. Deletion normally cascades, but action-point ledger rows currently block it. |
| [`auth_throttle_buckets`](#auth-throttle-buckets) | Authentication and identity | Every authentication attempt performs an `INSERT ... ON CONFLICT DO UPDATE` against `(key_digest, action_kind)`, resetting or incrementing the… |
| [`authentication_audits`](#authentication-audits) | Authentication and identity | Authentication flows insert one row per event. Normal request paths do not mutate or join these rows; reporting reads are time/account oriente… |
| [`player_character_audits`](#player-character-audits) | Account character library | Application-service mutations append retained audit history. Current browser update/delete handlers bypass that service, so those writes are not audited. |
| [`player_character_command_receipts`](#player-character-command-receipts) | Account character library | Application-service commands fingerprint retries and retain replay results. Current browser update/delete handlers bypass these receipts. |
| [`player_character_drafts`](#player-character-drafts) | Account character library | Owner-scoped resumable drafts use optimistic revisions. Cleanup deletes immediately at `expires_at`; no separate post-expiry retention deadline exists. |
| [`player_characters`](#player-characters) | Account character library | Owner-scoped level-less library records. Current UI update/delete calls repositories directly; active campaign-instance references restrict deletion. |
| [`campaign_character_instances`](#campaign-character-instances) | Campaign aggregate and membership | Assignment verifies active membership, loads the owned source character, creates the runtime hero, and inserts this bridge in one transaction.… |
| [`campaign_content_pins`](#campaign-content-pins) | Campaign aggregate and membership | Load is by the one-row-per-campaign primary key. On first use, pin creation locks the campaign, captures trusted server-side content metadata,… |
| [`campaign_invitations`](#campaign-invitations) | Campaign aggregate and membership | GM authorization is checked through `campaign_memberships` before insert/revoke. Redemption atomically updates only a matching, unexpired, una… |
| [`campaign_memberships`](#campaign-memberships) | Campaign aggregate and membership | Nearly every member-scoped loader first probes `(campaign_session_id, account_id, state = active)`. Campaign rosters scan by campaign; a user… |
| [`campaign_sessions`](#campaign-sessions) | Campaign aggregate and membership | Central aggregate locked for turn commits and lifecycle operations. Hard deletion is blocked while a custom-action ledger row references it. |
| [`campaign_deletion_preparations`](#campaign-deletion-preparations) | Campaign lifecycle and lobby | Preparation inserts/loads by `(owner_key, campaign_session_id, deletion_id)`. Confirmation locks the unexpired row and verifies captured campa… |
| [`campaign_deletion_tombstones`](#campaign-deletion-tombstones) | Campaign lifecycle and lobby | Deletion inserts the marker in the same transaction before removing the campaign. A blocking action-point ledger FK rolls back both operations. |
| [`campaign_lifecycle_audits`](#campaign-lifecycle-audits) | Campaign lifecycle and lobby | Lifecycle transactions append a row at the resulting lifecycle revision. Export/history and operational metrics read by campaign and chronolog… |
| [`campaign_lifecycle_receipts`](#campaign-lifecycle-receipts) | Campaign lifecycle and lobby | Lifecycle commands probe by `(owner_key, campaign_session_id, idempotency_key)`, verify the fingerprint and expected lifecycle revision, and s… |
| [`campaign_play_session_participants`](#campaign-play-session-participants) | Campaign lifecycle and lobby | Schema-ready, not yet used by production Rust SQL. Intended access is by `(play_session_id, account_id)`, with roster scans by play session an… |
| [`campaign_play_sessions`](#campaign-play-sessions) | Campaign lifecycle and lobby | Lifecycle code lists by campaign/owner, probes for one open (`waiting` or `active`) row under lock, inserts a waiting session, and closes it a… |
| [`campaign_private_recaps`](#campaign-private-recaps) | Campaign lifecycle and lobby | Generation first verifies campaign ownership/revision, reads a turn-audit range, derives a minimized body, and inserts an idempotent row. Late… |
| [`campaign_turn_states`](#campaign-turn-states) | Campaign lifecycle and lobby | Schema-ready, not yet used by production Rust SQL. Intended access is a primary-key load/lock by `play_session_id`, optimistic update by `revi… |
| [`lobby_command_receipts`](#lobby-command-receipts) | Campaign lifecycle and lobby | Schema-ready, not yet used by production Rust SQL. Intended access is an exact probe/insert on `(play_session_id, idempotency_key)`, with fing… |
| [`turn_control_audits`](#turn-control-audits) | Campaign lifecycle and lobby | Schema-ready, not yet used by production Rust SQL. Intended writes accompany turn-state transitions; history reads use `(play_session_id, crea… |
| [`characters`](#characters) | Core campaign state and turns | Creation inserts campaign-associated snapshots. Turn commit loads each changed row `FOR UPDATE`, verifies revision and campaign ownership, the… |
| [`command_receipts`](#command-receipts) | Core campaign state and turns | Before mutation, the repository probes `(campaign_session_id, idempotency_key)` and compares the request fingerprint. Successful commit insert… |
| [`turn_audits`](#turn-audits) | Core campaign state and turns | Atomic turn commit inserts exactly one row after locking aggregates. History, recaps, generation origin checks, presentation publication, insp… |
| [`encounter_reward_claims`](#encounter-reward-claims) | Campaign runtime heroes and rewards | Reward application probes the campaign/encounter/character key, locks authoritative encounter/hero state, derives tier and XP server-side, upd… |
| [`hero_audits`](#hero-audits) | Campaign runtime heroes and rewards | Hero transactions append at a subject revision; replay receipts and encounter claims reference the audit. History reads use campaign/subject o… |
| [`hero_characters`](#hero-characters) | Campaign runtime heroes and rewards | Authoritative runtime state loaded by hero/campaign-owner scope and locked for progression. Custom-action ledger references block hero deletion. |
| [`hero_command_receipts`](#hero-command-receipts) | Campaign runtime heroes and rewards | The composite key `(scope_kind, scope_id, idempotency_key)` supports exact replay across several command kinds. Inserts occur in the same tran… |
| [`hero_creation_drafts`](#hero-creation-drafts) | Campaign runtime heroes and rewards | Drafts are created and loaded by campaign/owner, selected by latest unexpired update, and saved with optimistic revision. Completion/deletion… |
| [`generation_attempts`](#generation-attempts) | Generated content queue and governance | Claim inserts a running attempt; heartbeats and completion/failure update that exact lease-token row. Job reclaim closes expired attempts befo… |
| [`generation_governance_diagnostics`](#generation-governance-diagnostics) | Generated content queue and governance | Governance inserts on denial; operations aggregate by purpose/scope/dimension. Cleanup deletes rows after the default 14-day deadline using bo… |
| [`generation_governance_receipts`](#generation-governance-receipts) | Generated content queue and governance | Governance probes by campaign/purpose/key or `job_id`, sums active/spent reservations for limits, inserts a reserved row with the job, then up… |
| [`generation_jobs`](#generation-jobs) | Generated content queue and governance | Enqueue locks the campaign revision and inserts once per `(campaign, purpose, idempotency_key)`. Workers claim eligible rows with `FOR UPDATE… |
| [`generated_assets`](#generated-assets) | Generated presentations and scene images | Generation completion inserts by artifact ID and campaign; campaign views list by campaign/turn. Scene-image publication upserts the generic r… |
| [`generated_text_presentation_receipts`](#generated-text-presentation-receipts) | Generated presentations and scene images | Publication inserts by `(campaign_session_id, client_idempotency_key)` and also versions by origin turn. Retry paths load the receipt to retur… |
| [`generated_text_presentations`](#generated-text-presentations) | Generated presentations and scene images | Publication validates the running job/attempt and origin turn under locks, supersedes the previous selected version, inserts a new version, an… |
| [`scene_image_artifacts`](#scene-image-artifacts) | Generated presentations and scene images | Publication upserts by artifact ID while the generation job is still authoritative. Reads join `generation_jobs` for visibility/retention chec… |
| [`scene_image_quarantines`](#scene-image-quarantines) | Generated presentations and scene images | Image validation inserts a minimized row for failures. Cleanup selects expired rows ordered by deadline, removes external bytes first, then de… |
| [`typed_intent_command_receipts`](#typed-intent-command-receipts) | Generated presentations and scene images | Validation inserts a `pending` receipt keyed by campaign/client key with resolved intent and evidence. The mechanics transaction later updates… |
| [`campaign_inspiration_allowed_sensitivities`](#campaign-inspiration-allowed-sensitivities) | Private inspiration and consent | Selection loads the full set by campaign and intersects it with source/grant sensitivity sets. Rows are replaced/inserted as part of safety se… |
| [`campaign_inspiration_excluded_participants`](#campaign-inspiration-excluded-participants) | Private inspiration and consent | Selection scans by campaign and excludes sources connected through `private_inspiration_source_participants`. It is managed with safety setup… |
| [`campaign_inspiration_excluded_topics`](#campaign-inspiration-excluded-topics) | Private inspiration and consent | Selection scans by campaign and removes matching candidates. It is managed with safety setup and cascades with the settings row. |
| [`campaign_inspiration_lines`](#campaign-inspiration-lines) | Private inspiration and consent | Selection loads the set by campaign and rejects matching material. It is a compact composite-key child of campaign inspiration settings and is… |
| [`campaign_inspiration_settings`](#campaign-inspiration-settings) | Private inspiration and consent | Setup and operator/player controls insert or update the campaign row under revision checks. Selection locks/reads it with its child allow/excl… |
| [`campaign_inspiration_veils`](#campaign-inspiration-veils) | Private inspiration and consent | Selection loads the set by campaign to constrain transformation/presentation. It is managed as a composite-key child of campaign inspiration s… |
| [`private_inspiration_command_receipts`](#private-inspiration-command-receipts) | Private inspiration and consent | Each command probes `(campaign_session_id, idempotency_key)`, validates operation/fingerprint, and inserts a replay response in the same trans… |
| [`private_inspiration_consent_grants`](#private-inspiration-consent-grants) | Private inspiration and consent | Grant/revoke workflows insert or update under operator and participant evidence checks. Candidate selection joins active, unexpired grants by… |
| [`private_inspiration_consent_sensitivities`](#private-inspiration-consent-sensitivities) | Private inspiration and consent | Grant creation inserts the set; candidate selection joins it to source and campaign safety codes. Rows cascade when the parent grant is removed. |
| [`private_inspiration_deletion_tombstones`](#private-inspiration-deletion-tombstones) | Private inspiration and consent | Participant deletion first removes protected source bodies, then revokes grants, quarantines sources, cancels/redacts derived work, marks veri… |
| [`private_inspiration_derived_work`](#private-inspiration-derived-work) | Private inspiration and consent | Selection inserts pending work; cancellation/privacy controls update state; text publication locks the row and marks it completed with artifac… |
| [`private_inspiration_global_command_receipts`](#private-inspiration-global-command-receipts) | Private inspiration and consent | Operator commands probe by key, compare the request fingerprint, and insert the serialized response once. Rows are immutable and intentionally… |
| [`private_inspiration_global_control`](#private-inspiration-global-control) | Private inspiration and consent | Runtime selection/publication reads the singleton, sometimes `FOR SHARE`; offline/operator control locks and updates revision, disabled flag,… |
| [`private_inspiration_participants`](#private-inspiration-participants) | Private inspiration and consent | Offline/operator registration inserts versioned verification evidence; revocation updates the row. Consent/source/veto workflows resolve by `p… |
| [`private_inspiration_privacy_audits`](#private-inspiration-privacy-audits) | Private inspiration and consent | Consent, veto, deletion, selection, derived-work, and publication paths insert result-coded events. Ordinary runtime paths do not update rows;… |
| [`private_inspiration_restricted_access_audits`](#private-inspiration-restricted-access-audits) | Private inspiration and consent | Restricted tools insert an idempotent, fingerprinted result record and query it for exact replay/history. The row contains only IDs/digests/pu… |
| [`private_inspiration_runtime_facts`](#private-inspiration-runtime-facts) | Private inspiration and consent | Offline approval inserts the bounded fact list. Runtime selection reads all facts by `(source_id, source_version)` ordered by `fact_index` to… |
| [`private_inspiration_runtime_prompts`](#private-inspiration-runtime-prompts) | Private inspiration and consent | Offline approval inserts/upserts the projection. Runtime selection reads enabled rows and joins facts plus consent/safety mappings. The protec… |
| [`private_inspiration_selection_audits`](#private-inspiration-selection-audits) | Private inspiration and consent | Selection first checks the campaign/key idempotency path, computes eligibility, inserts this audit, advances settings cursor, records usage, a… |
| [`private_inspiration_source_media`](#private-inspiration-source-media) | Private inspiration and consent | Registration inserts allowed media values. Candidate selection and consent checks join by source/version/media; presentation fails closed if t… |
| [`private_inspiration_source_participants`](#private-inspiration-source-participants) | Private inspiration and consent | Registration inserts the source’s participant set. Consent and exclusion checks join on the composite source key and participant ID. Rows are… |
| [`private_inspiration_source_sensitivities`](#private-inspiration-source-sensitivities) | Private inspiration and consent | Registration inserts the label set; selection compares it with campaign allowances, lines/veils, and grant sensitivity scope. Rows are immutab… |
| [`private_inspiration_source_themes`](#private-inspiration-source-themes) | Private inspiration and consent | Registration inserts theme mappings. Selection joins the campaign’s pinned/current theme against `(source_id, source_version, theme_pack_id)`… |
| [`private_inspiration_source_usage`](#private-inspiration-source-usage) | Private inspiration and consent | Selection inserts one row per successful selection. Future candidate scans use campaign/source and `next_eligible_turn` to enforce cooldown; r… |
| [`private_inspiration_sources`](#private-inspiration-sources) | Private inspiration and consent | Offline registration inserts `(source_id, source_version)` plus child mappings; review updates screening state/evidence. Selection joins runti… |
| [`private_inspiration_vetoes`](#private-inspiration-vetoes) | Private inspiration and consent | Control paths insert scoped veto records; selection probes active vetoes by campaign/participant/category/source. Rows preserve actor kind and… |
| [`custom_action_point_balances`](#custom-action-point-balances) | Custom action points | Unwired repository cache updated under row lock with no FKs. Upserts retain the original `play_session_id` for a character/campaign key. |
| [`custom_action_point_ledger`](#custom-action-point-ledger) | Custom action points | Unwired append-only ledger with globally weak replay identity. Default `NO ACTION` FKs block account, campaign, and runtime-hero deletion. |
| [`operator_recovery_status`](#operator-recovery-status) | Operations and recovery | Migration seeds exactly one row. Server operations snapshots read it; backup/restore tooling is expected to update it after verified operation… |

## Authentication and identity

### `account_sessions`

**Purpose.** Server-side login-session registry. Only digests of the bearer and CSRF tokens are stored.

**Access pattern.** The hot path looks up an unrevoked, unexpired row by unique `token_digest`, then advances `last_seen_at` and the bounded idle deadline. Sessions are inserted at login, soft-revoked individually or in excess-session batches, and hard-deleted after absolute expiry. Rows cascade with the account.

**Migration source(s).** `migrations/0025_accounts_and_sessions.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/auth.rs:674` (SELECT), `crates/game-server/src/repository/auth.rs:82` (DELETE/INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique; checked | Stable application-generated identifier for the `account_sessions` row. |
| `account_id` | `text` | required; composite unique; FK → accounts(id) ON DELETE CASCADE | Account participating in or owning the scoped relation. |
| `token_digest` | `text` | required; unique; checked | Deterministic digest of token, used for integrity/equality checks without retaining the raw input. |
| `csrf_digest` | `text` | required; unique; checked | Deterministic digest of csrf, used for integrity/equality checks without retaining the raw input. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP`; checked | Database timestamp when the row was inserted. |
| `last_seen_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP`; checked | Most recent successful use of the login session. |
| `idle_expires_at` | `timestamp with time zone` | required; checked | Sliding inactivity deadline; authentication advances it without exceeding absolute expiry. |
| `absolute_expires_at` | `timestamp with time zone` | required; checked | Hard login-session deadline that cannot be extended. |
| `revoked_at` | `timestamp with time zone` | nullable; checked | Timestamp when this session/invitation was revoked; null while active. |

<details>
<summary>Exact table constraints</summary>

- `account_sessions_pkey` — `PRIMARY KEY (id)`
- `account_sessions_csrf_digest_key` — `UNIQUE (csrf_digest)`
- `account_sessions_id_account_id_key` — `UNIQUE (id, account_id)`
- `account_sessions_token_digest_key` — `UNIQUE (token_digest)`
- `account_sessions_account_id_fkey` — `FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `account_sessions_check` — `CHECK (last_seen_at >= created_at)`
- `account_sessions_check1` — `CHECK (idle_expires_at > created_at)`
- `account_sessions_check2` — `CHECK (absolute_expires_at > created_at)`
- `account_sessions_check3` — `CHECK (idle_expires_at <= absolute_expires_at)`
- `account_sessions_check4` — `CHECK (revoked_at IS NULL OR revoked_at >= created_at)`
- `account_sessions_csrf_digest_check` — `CHECK (csrf_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `account_sessions_id_check` — `CHECK (id ~ '^session:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'::text)`
- `account_sessions_token_digest_check` — `CHECK (token_digest ~ '^sha256:[0-9a-f]{64}$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `account_sessions_account_active_idx` — `CREATE INDEX account_sessions_account_active_idx ON public.account_sessions USING btree (account_id, created_at DESC, id) WHERE (revoked_at IS NULL)`
- `account_sessions_active_lookup_idx` — `CREATE INDEX account_sessions_active_lookup_idx ON public.account_sessions USING btree (token_digest, idle_expires_at, absolute_expires_at) WHERE (revoked_at IS NULL)`
- `account_sessions_csrf_digest_key` — `CREATE UNIQUE INDEX account_sessions_csrf_digest_key ON public.account_sessions USING btree (csrf_digest)`
- `account_sessions_expiry_cleanup_idx` — `CREATE INDEX account_sessions_expiry_cleanup_idx ON public.account_sessions USING btree (absolute_expires_at, idle_expires_at)`
- `account_sessions_id_account_id_key` — `CREATE UNIQUE INDEX account_sessions_id_account_id_key ON public.account_sessions USING btree (id, account_id)`
- `account_sessions_token_digest_key` — `CREATE UNIQUE INDEX account_sessions_token_digest_key ON public.account_sessions USING btree (token_digest)`

</details>

### `accounts`

**Purpose.** Application identity record for a human or local bootstrap account. It holds login-facing identity and password-verifier state, but campaign authorization is expressed through memberships rather than directly on this row.

**Access pattern.** Login loads by the unique normalized email; authenticated/account-summary paths load by `id`. Registration inserts the account (often in the same transaction as its first session), and password changes update verifier fields. Account deletion is designed to cascade to sessions, memberships, owned library characters, and account-owned campaigns, but a referencing `custom_action_point_ledger` row currently blocks the delete through its default `NO ACTION` account FK.

**Migration source(s).** `migrations/0025_accounts_and_sessions.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application/player_characters.rs:736` (INSERT), `crates/game-server/src/auth.rs:674` (SELECT), `crates/game-server/src/repository/action_points.rs:321` (INSERT), `crates/game-server/src/repository/auth.rs:24` (DELETE/INSERT/SELECT/UPDATE), `crates/game-server/src/repository/memberships.rs:1224` (INSERT), `crates/game-server/src/repository/player_characters.rs:891` (INSERT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `accounts` row. |
| `normalized_email` | `text` | nullable; unique; checked | Canonical normalized login email used for unique account lookup; nullable for non-login/bootstrap accounts. |
| `display_name` | `text` | required; checked | User-facing name, validated and uniqueness-scoped according to the table constraints. |
| `password_phc` | `text` | nullable; checked | Password verifier encoded in PHC string format; nullable when login is disabled. Plaintext passwords are never stored. |
| `login_enabled` | `boolean` | required; default `true`; checked | Whether password/session login is permitted for the account (false for bootstrap/service-like rows). |
| `password_changed_at` | `timestamp with time zone` | nullable; checked | Timestamp of the latest password-verifier replacement; used to invalidate older sessions. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `accounts_pkey` — `PRIMARY KEY (id)`
- `accounts_normalized_email_key` — `UNIQUE (normalized_email)`
- `accounts_check` — `CHECK (login_enabled AND normalized_email IS NOT NULL AND password_phc IS NOT NULL AND password_changed_at IS NOT NULL OR NOT login_enabled AND normalized_email IS NULL AND password_phc IS NULL AND password_changed_at IS NULL)`
- `accounts_display_name_check` — `CHECK (octet_length(display_name) >= 1 AND octet_length(display_name) <= 200 AND display_name = btrim(display_name))`
- `accounts_id_check` — `CHECK (id = 'account:local'::text OR id ~ '^account:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'::text)`
- `accounts_normalized_email_check` — `CHECK (normalized_email IS NULL OR octet_length(normalized_email) >= 3 AND octet_length(normalized_email) <= 320 AND normalized_email = lower(btrim(normalized_email)) AND normalized_email !~ '[[:space:]]'::text AND normalized_email ~ '^[^@]+@[^@]+$'::text)`
- `accounts_password_phc_check` — `CHECK (password_phc IS NULL OR octet_length(password_phc) >= 32 AND octet_length(password_phc) <= 1024 AND password_phc ~~ '$argon2id$%'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `accounts_normalized_email_key` — `CREATE UNIQUE INDEX accounts_normalized_email_key ON public.accounts USING btree (normalized_email)`

</details>

### `auth_throttle_buckets`

**Purpose.** Rolling authentication rate-limit state keyed by a privacy-preserving digest and action type.

**Access pattern.** Every authentication attempt performs an `INSERT ... ON CONFLICT DO UPDATE` against `(key_digest, action_kind)`, resetting or incrementing the window and computing `blocked_until`. This is a small, overwrite-heavy operational table; raw emails/IPs must never be stored.

**Migration source(s).** `migrations/0025_accounts_and_sessions.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/auth.rs:393` (INSERT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `key_digest` | `text` | required; PK component; checked | Deterministic digest of key, used for integrity/equality checks without retaining the raw input. |
| `action_kind` | `text` | required; PK component; checked | Authentication action category that partitions throttle state. |
| `window_started_at` | `timestamp with time zone` | required; checked | Start of the current rolling throttle window. |
| `attempt_count` | `integer` | required; checked | Number of authentication attempts recorded in the current throttle window. |
| `blocked_until` | `timestamp with time zone` | nullable; checked | Timestamp until which this throttle key/action must be rejected; null when the bucket is not blocked. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `auth_throttle_buckets_pkey` — `PRIMARY KEY (key_digest, action_kind)`
- `auth_throttle_buckets_action_kind_check` — `CHECK (action_kind = ANY (ARRAY['login'::text, 'signup'::text]))`
- `auth_throttle_buckets_attempt_count_check` — `CHECK (attempt_count >= 0 AND attempt_count <= 1000000)`
- `auth_throttle_buckets_check` — `CHECK (blocked_until IS NULL OR blocked_until >= window_started_at)`
- `auth_throttle_buckets_key_digest_check` — `CHECK (key_digest ~ '^hmac-sha256:[0-9a-f]{64}$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `auth_throttle_buckets_cleanup_idx` — `CREATE INDEX auth_throttle_buckets_cleanup_idx ON public.auth_throttle_buckets USING btree (updated_at, blocked_until)`

</details>

### `authentication_audits`

**Purpose.** Append-only, minimized record of authentication/security outcomes for operations and incident review.

**Access pattern.** Authentication flows insert one row per event. Normal request paths do not mutate or join these rows; reporting reads are time/account oriented. `account_id` is nullable and becomes null on account deletion so the security audit survives without retaining the deleted principal.

**Migration source(s).** `migrations/0025_accounts_and_sessions.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/auth.rs:353` (INSERT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `authentication_audits` row. |
| `account_id` | `text` | nullable; FK → accounts(id) ON DELETE SET NULL | Account participating in or owning the scoped relation. |
| `event_kind` | `text` | required; checked | Controlled event discriminator used for audit interpretation and metrics. |
| `outcome_class` | `text` | required; checked | Coarse authentication result class suitable for security metrics. |
| `correlation_id` | `text` | required; checked | Request/trace correlation identifier used to connect audits and operational logs. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `authentication_audits_pkey` — `PRIMARY KEY (id)`
- `authentication_audits_account_id_fkey` — `FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE SET NULL`
- `authentication_audits_correlation_id_check` — `CHECK (octet_length(correlation_id) >= 1 AND octet_length(correlation_id) <= 128 AND correlation_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `authentication_audits_event_kind_check` — `CHECK (event_kind = ANY (ARRAY['signup'::text, 'login'::text, 'logout'::text, 'session_expired'::text, 'password_rehashed'::text]))`
- `authentication_audits_id_check` — `CHECK (id ~ '^auth-audit:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'::text)`
- `authentication_audits_outcome_class_check` — `CHECK (outcome_class = ANY (ARRAY['success'::text, 'invalid_credentials'::text, 'throttled'::text, 'invalid_request'::text, 'internal_failure'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `authentication_audits_account_time_idx` — `CREATE INDEX authentication_audits_account_time_idx ON public.authentication_audits USING btree (account_id, created_at DESC, id)`
- `authentication_audits_time_idx` — `CREATE INDEX authentication_audits_time_idx ON public.authentication_audits USING btree (created_at DESC, id)`

</details>


## Account character library

### `player_character_audits`

**Purpose.** Append-only history of library-character changes and deletions.

**Access pattern.** The application service writes these after character mutations and rebinds both character and account IDs from server authority. Reads are mainly operational/tests. On character deletion `character_id` is set null while `owner_account_id`, action, revision, and minimized audit payload remain, preserving a non-identifying retention record. The current browser display-name and delete handlers call repository mutations directly, bypassing these inserts; application-service mutation and audit calls are also separate transactions rather than one atomic unit.

**Migration source(s).** `migrations/0026_player_character_library.sql`, `migrations/0027_player_character_audit_retention.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application/player_characters.rs:957` (SELECT), `crates/game-server/src/repository/player_characters.rs:434` (INSERT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `bigint` | required; default `nextval('player_character_audits_id_seq')`; PK | Database-generated bigint surrogate identifier for the `player_character_audits` row. |
| `character_id` | `text` | nullable; FK → player_characters(id) ON DELETE SET NULL | Identifier for the associated character; used to scope, join, or correlate this row. |
| `owner_account_id` | `text` | required; FK → accounts(id) ON DELETE CASCADE | Account that owns the row; server-derived and used as the authorization partition. |
| `action` | `text` | required; checked | Library-character audit action name. |
| `revision` | `bigint` | required | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `audit_json` | `jsonb` | required | Minimized action-specific audit payload; retained independently of the deleted character row. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `player_character_audits_pkey` — `PRIMARY KEY (id)`
- `player_character_audits_character_id_fkey` — `FOREIGN KEY (character_id) REFERENCES player_characters(id) ON DELETE SET NULL`
- `player_character_audits_owner_account_id_fkey` — `FOREIGN KEY (owner_account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `player_character_audits_action_check` — `CHECK (octet_length(action) >= 1 AND octet_length(action) <= 64)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `idx_player_character_audits_character` — `CREATE INDEX idx_player_character_audits_character ON public.player_character_audits USING btree (character_id, created_at DESC)`

</details>

### `player_character_command_receipts`

**Purpose.** Durable idempotency receipts for library-character commands.

**Access pattern.** Application-service commands probe by `(character_id, idempotency_key, owner_account_id)`, compare request fingerprints, and insert exactly once. Receipts survive character deletion (there is no final FK from `character_id`) so an old key cannot recreate a mutation; account deletion normally cascades them. Current browser display-name and delete handlers bypass the application service, so those direct repository writes do not create receipts. Mutation, audit, and receipt inserts are separate transactions and can diverge after a partial failure.

**Migration source(s).** `migrations/0026_player_character_library.sql`, `migrations/0027_player_character_audit_retention.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application/player_characters.rs:968` (SELECT), `crates/game-server/src/repository/player_characters.rs:472` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `bigint` | required; default `nextval('player_character_command_receipts_id_seq')`; PK | Database-generated bigint surrogate identifier for the `player_character_command_receipts` row. |
| `owner_account_id` | `text` | required; FK → accounts(id) ON DELETE CASCADE | Account that owns the row; server-derived and used as the authorization partition. |
| `character_id` | `text` | required; composite unique | Identifier for the associated character; used to scope, join, or correlate this row. |
| `idempotency_key` | `text` | required; composite unique; checked | Opaque client/operator retry key within the table’s documented scope. |
| `command_kind` | `text` | required; checked | Controlled command discriminator used during idempotent replay validation. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `result_revision` | `bigint` | required | Revision returned after the idempotent command completed. |
| `response_json` | `jsonb` | required | Bounded structured replay result returned for an exact duplicate command. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `player_character_command_receipts_pkey` — `PRIMARY KEY (id)`
- `player_character_command_recei_character_id_idempotency_key_key` — `UNIQUE (character_id, idempotency_key)`
- `player_character_command_receipts_owner_account_id_fkey` — `FOREIGN KEY (owner_account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `player_character_command_receipts_command_kind_check` — `CHECK (octet_length(command_kind) >= 1 AND octet_length(command_kind) <= 64)`
- `player_character_command_receipts_idempotency_key_check` — `CHECK (idempotency_key ~ '^[a-zA-Z0-9_-]{1,128}$'::text)`
- `player_character_command_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `idx_player_character_receipts_owner` — `CREATE INDEX idx_player_character_receipts_owner ON public.player_character_command_receipts USING btree (owner_account_id, character_id)`
- `player_character_command_recei_character_id_idempotency_key_key` — `CREATE UNIQUE INDEX player_character_command_recei_character_id_idempotency_key_key ON public.player_character_command_receipts USING btree (character_id, idempotency_key)`

</details>

### `player_character_drafts`

**Purpose.** Expiring, resumable character-library creation workflow owned by an account.

**Access pattern.** Create/load/save/commit/delete operations always include `owner_account_id`. Saves use expected `revision`; commit links the resulting library character. A maintenance delete removes rows immediately when `expires_at` has passed, and account deletion cascades all drafts. Despite retention comments/constants elsewhere, this table has no distinct 30-day post-expiry retention deadline.

**Migration source(s).** `migrations/0026_player_character_library.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/player_characters.rs:234` (DELETE/INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `player_character_drafts` row. |
| `owner_account_id` | `text` | required; FK → accounts(id) ON DELETE CASCADE | Account that owns the row; server-derived and used as the authorization partition. |
| `revision` | `bigint` | required; default `0` | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `expires_at` | `timestamp with time zone` | required | Deadline after which the row/workflow is no longer valid. |
| `step` | `text` | required; default `'campaign_theme'` | Current character-creation workflow step. |
| `choices_json` | `jsonb` | nullable; checked | Partial reusable-character choices for the current creation step; nullable before the first save. |
| `reviewed` | `boolean` | required; default `false`; checked | Whether the draft passed review and may be linked to a committed character. |
| `committed_character_id` | `text` | nullable; FK → player_characters(id) ON DELETE SET NULL; checked | Identifier for the associated committed character; used to scope, join, or correlate this row. |
| `schema_version` | `integer` | required; default `1`; checked | Version of the row or serialized contract required to interpret this record safely. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `player_character_drafts_pkey` — `PRIMARY KEY (id)`
- `player_character_drafts_committed_character_id_fkey` — `FOREIGN KEY (committed_character_id) REFERENCES player_characters(id) ON DELETE SET NULL`
- `player_character_drafts_owner_account_id_fkey` — `FOREIGN KEY (owner_account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `player_character_drafts_check` — `CHECK (committed_character_id IS NULL OR reviewed = true AND choices_json IS NOT NULL)`
- `player_character_drafts_id_check` — `CHECK (id ~ '^draft:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'::text)`
- `player_character_drafts_schema_version_check` — `CHECK (schema_version = 1)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `idx_player_character_drafts_owner` — `CREATE INDEX idx_player_character_drafts_owner ON public.player_character_drafts USING btree (owner_account_id, updated_at DESC)`

</details>

### `player_characters`

**Purpose.** Level-less, account-owned reusable character definition. Campaign runtime state such as level, XP, HP, and resources does not belong here.

**Access pattern.** All reads and writes are scoped by `(owner_account_id, id)` to prevent cross-account enumeration. Lists use `owner_account_id` ordered by recent update; display-name changes use optimistic `revision`; deletion is owner-scoped and leaves retained audit/receipt evidence according to their FK rules. The current browser handlers invoke repository update/delete methods directly rather than the audited, idempotent application workflow. Deletion is also blocked while `campaign_character_instances.source_player_character_id` references the row through `ON DELETE RESTRICT`.

**Migration source(s).** `migrations/0026_player_character_library.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/memberships.rs:1247` (INSERT), `crates/game-server/src/repository/player_characters.rs:69` (DELETE/INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `player_characters` row. |
| `owner_account_id` | `text` | required; FK → accounts(id) ON DELETE CASCADE; checked | Account that owns the row; server-derived and used as the authorization partition. |
| `revision` | `bigint` | required; default `0` | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `display_name` | `text` | required; checked | User-facing name, validated and uniqueness-scoped according to the table constraints. |
| `choices_json` | `jsonb` | required | Level-less reusable character choices (identity, ancestry/class/build selections); excludes campaign runtime stats. |
| `schema_version` | `integer` | required; default `1`; checked | Version of the row or serialized contract required to interpret this record safely. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `player_characters_pkey` — `PRIMARY KEY (id)`
- `player_characters_owner_account_id_fkey` — `FOREIGN KEY (owner_account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `player_characters_check` — `CHECK (owner_account_id <> 'account:local'::text OR id ~~ 'character:local-%'::text)`
- `player_characters_display_name_check` — `CHECK (octet_length(display_name) >= 1 AND octet_length(display_name) <= 200 AND display_name = btrim(display_name))`
- `player_characters_id_check` — `CHECK (id ~ '^character:[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$'::text)`
- `player_characters_schema_version_check` — `CHECK (schema_version = 1)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `idx_player_characters_owner_display_name` — `CREATE UNIQUE INDEX idx_player_characters_owner_display_name ON public.player_characters USING btree (owner_account_id, lower(display_name))`
- `idx_player_characters_owner_updated` — `CREATE INDEX idx_player_characters_owner_updated ON public.player_characters USING btree (owner_account_id, updated_at DESC, id)`

</details>


## Campaign aggregate and membership

### `campaign_character_instances`

**Purpose.** Bridge from an account library character to its campaign-specific runtime hero. It snapshots source identity/digest while linking the authoritative `hero_characters` runtime row.

**Access pattern.** Assignment verifies active membership, loads the owned source character, creates the runtime hero, and inserts this bridge in one transaction. Active lookup is by `(campaign_session_id, account_id, state)`; a partial unique index allows at most one active character slot per account/campaign. Source library deletion is restricted while referenced. There is no composite FK to membership, and member removal currently does not retire the instance.

**Migration source(s).** `migrations/0028_campaign_memberships.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/memberships.rs:782` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `account_id` | `text` | required; FK → accounts(id) ON DELETE CASCADE | Account participating in or owning the scoped relation. |
| `instance_id` | `text` | required; PK component; checked | Identifier for the associated instance; used to scope, join, or correlate this row. |
| `source_player_character_id` | `text` | required; FK → player_characters(id) ON DELETE RESTRICT | Identifier for the associated source player character; used to scope, join, or correlate this row. |
| `runtime_hero_character_id` | `text` | required; FK → hero_characters(id) ON DELETE CASCADE | Identifier for the associated runtime hero character; used to scope, join, or correlate this row. |
| `source_display_name` | `text` | required; checked | Snapshot of the library character name at campaign instantiation time. |
| `source_choices_digest` | `text` | required; checked | Deterministic digest of source choices, used for integrity/equality checks without retaining the raw input. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `retired_at` | `timestamp with time zone` | nullable; checked | Timestamp when the campaign character instance was retired. |

<details>
<summary>Exact table constraints</summary>

- `campaign_character_instances_pkey` — `PRIMARY KEY (campaign_session_id, instance_id)`
- `campaign_character_instances_account_id_fkey` — `FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `campaign_character_instances_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_character_instances_runtime_hero_character_id_fkey` — `FOREIGN KEY (runtime_hero_character_id) REFERENCES hero_characters(id) ON DELETE CASCADE`
- `campaign_character_instances_source_player_character_id_fkey` — `FOREIGN KEY (source_player_character_id) REFERENCES player_characters(id) ON DELETE RESTRICT`
- `campaign_character_instances_check` — `CHECK (state = 'active'::text AND retired_at IS NULL OR state = 'retired'::text AND retired_at IS NOT NULL)`
- `campaign_character_instances_instance_id_check` — `CHECK (octet_length(instance_id) >= 1 AND octet_length(instance_id) <= 128 AND instance_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_character_instances_source_choices_digest_check` — `CHECK (source_choices_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_character_instances_source_display_name_check` — `CHECK (octet_length(source_display_name) >= 1 AND octet_length(source_display_name) <= 200)`
- `campaign_character_instances_state_check` — `CHECK (state = ANY (ARRAY['active'::text, 'retired'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_character_instances_account_idx` — `CREATE INDEX campaign_character_instances_account_idx ON public.campaign_character_instances USING btree (account_id, campaign_session_id)`
- `campaign_character_instances_active_per_account_idx` — `CREATE UNIQUE INDEX campaign_character_instances_active_per_account_idx ON public.campaign_character_instances USING btree (campaign_session_id, account_id) WHERE (state = 'active'::text)`
- `campaign_character_instances_source_idx` — `CREATE INDEX campaign_character_instances_source_idx ON public.campaign_character_instances USING btree (source_player_character_id, campaign_session_id)`

</details>

### `campaign_content_pins`

**Purpose.** Immutable sealed snapshot of the content catalog, rules, prompt, and policy identities a campaign must continue using.

**Access pattern.** Load is by the one-row-per-campaign primary key. On first use, pin creation locks the campaign, captures trusted server-side content metadata, inserts once, and clears legacy eligibility. Inspiration and game execution read the pin to fail closed on catalog or policy drift; normal code does not update the row.

**Migration source(s).** `migrations/0007_campaign_content_pins.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application.rs:2722` (UPDATE), `crates/game-server/src/application/hero.rs:2593` (DELETE/SELECT), `crates/game-server/src/repository/inspiration.rs:2756` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1732` (INSERT/SELECT), `crates/game-server/src/repository/pins.rs:28` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `seal_reason` | `text` | required; checked | Controlled reason explaining why/when the campaign content snapshot was sealed. |
| `payload_json` | `jsonb` | required; checked | Canonical sealed content/policy pin snapshot used to reproduce and validate campaign execution. |
| `legacy_source_json` | `jsonb` | nullable; checked | Optional pre-seal source snapshot retained only for eligible legacy campaign migration/audit. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `campaign_content_pins_pkey` — `PRIMARY KEY (campaign_session_id)`
- `campaign_content_pins_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_content_pins_check` — `CHECK ((seal_reason = 'legacy_digest_alias'::text) = (legacy_source_json IS NOT NULL))`
- `campaign_content_pins_legacy_source_json_check` — `CHECK (legacy_source_json IS NULL OR jsonb_typeof(legacy_source_json) = 'object'::text AND octet_length(legacy_source_json::text) >= 2 AND octet_length(legacy_source_json::text) <= 8192)`
- `campaign_content_pins_payload_json_check` — `CHECK (jsonb_typeof(payload_json) = 'object'::text AND octet_length(payload_json::text) >= 2 AND octet_length(payload_json::text) <= 32768)`
- `campaign_content_pins_schema_version_check` — `CHECK (schema_version = 1)`
- `campaign_content_pins_seal_reason_check` — `CHECK (seal_reason = ANY (ARRAY['selected_theme'::text, 'legacy_selected_theme'::text, 'legacy_digest_alias'::text, 'legacy_default_rainbound'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_content_pins_created_idx` — `CREATE INDEX campaign_content_pins_created_idx ON public.campaign_content_pins USING btree (created_at, campaign_session_id)`

</details>

### `campaign_invitations`

**Purpose.** Time-bounded invitation to join a campaign, addressed by a digested email or join-code digest.

**Access pattern.** GM authorization is checked through `campaign_memberships` before insert/revoke. Redemption atomically updates only a matching, unexpired, unaccepted, unrevoked row, then inserts membership in the same transaction. Lookup is by invitation ID for display and by indexed digests for redemption. Expiry is enforced in predicates; no dedicated hard-delete cleanup path currently removes expired rows.

**Migration source(s).** `migrations/0028_campaign_memberships.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/memberships.rs:294` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `campaign_invitations` row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `inviter_account_id` | `text` | required; FK → accounts(id) ON DELETE CASCADE | Identifier for the associated inviter account; used to scope, join, or correlate this row. |
| `invitee_email_digest` | `text` | nullable; checked | Deterministic digest of invitee email, used for integrity/equality checks without retaining the raw input. |
| `join_code_digest` | `text` | nullable; checked | Deterministic digest of join code, used for integrity/equality checks without retaining the raw input. |
| `expires_at` | `timestamp with time zone` | required | Deadline after which the row/workflow is no longer valid. |
| `accepted_at` | `timestamp with time zone` | nullable; checked | Timestamp when the invitation/membership was accepted; null until acceptance. |
| `accepted_account_id` | `text` | nullable; FK → accounts(id) ON DELETE SET NULL; checked | Identifier for the associated accepted account; used to scope, join, or correlate this row. |
| `revoked_at` | `timestamp with time zone` | nullable; checked | Timestamp when this session/invitation was revoked; null while active. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `campaign_invitations_pkey` — `PRIMARY KEY (id)`
- `campaign_invitations_accepted_account_id_fkey` — `FOREIGN KEY (accepted_account_id) REFERENCES accounts(id) ON DELETE SET NULL`
- `campaign_invitations_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_invitations_inviter_account_id_fkey` — `FOREIGN KEY (inviter_account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `campaign_invitations_check` — `CHECK ((invitee_email_digest IS NULL) <> (join_code_digest IS NULL))`
- `campaign_invitations_check1` — `CHECK (accepted_at IS NULL AND accepted_account_id IS NULL AND revoked_at IS NULL OR accepted_at IS NOT NULL AND accepted_account_id IS NOT NULL AND revoked_at IS NULL OR accepted_at IS NULL AND accepted_account_id IS NULL AND revoked_at IS NOT NULL)`
- `campaign_invitations_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_invitations_invitee_email_digest_check` — `CHECK (invitee_email_digest IS NULL OR invitee_email_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_invitations_join_code_digest_check` — `CHECK (join_code_digest IS NULL OR join_code_digest ~ '^sha256:[0-9a-f]{64}$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_invitations_campaign_idx` — `CREATE INDEX campaign_invitations_campaign_idx ON public.campaign_invitations USING btree (campaign_session_id, created_at DESC, id)`
- `campaign_invitations_email_digest_idx` — `CREATE INDEX campaign_invitations_email_digest_idx ON public.campaign_invitations USING btree (invitee_email_digest, expires_at) WHERE ((invitee_email_digest IS NOT NULL) AND (accepted_at IS NULL) AND (revoked_at IS NULL))`
- `campaign_invitations_join_code_digest_idx` — `CREATE INDEX campaign_invitations_join_code_digest_idx ON public.campaign_invitations USING btree (join_code_digest, expires_at) WHERE ((join_code_digest IS NOT NULL) AND (accepted_at IS NULL) AND (revoked_at IS NULL))`

</details>

### `campaign_memberships`

**Purpose.** Account-to-campaign authorization relation, including game-master/player role and active/removed state.

**Access pattern.** Nearly every member-scoped loader first probes `(campaign_session_id, account_id, state = active)`. Campaign rosters scan by campaign; a user dashboard scans by account and joins `campaign_sessions`. Invite acceptance inserts a player row; removal is a soft state transition. A partial unique index enforces one active GM, but no database constraint requires that GM account to equal `campaign_sessions.owner_account_id`.

**Migration source(s).** `migrations/0028_campaign_memberships.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/memberships.rs:240` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `account_id` | `text` | required; PK component; FK → accounts(id) ON DELETE CASCADE | Account participating in or owning the scoped relation. |
| `role` | `text` | required; checked | Membership authorization role, constrained to the supported role set. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `inviter_account_id` | `text` | nullable; FK → accounts(id) ON DELETE SET NULL | Identifier for the associated inviter account; used to scope, join, or correlate this row. |
| `accepted_at` | `timestamp with time zone` | nullable; checked | Timestamp when the invitation/membership was accepted; null until acceptance. |
| `left_at` | `timestamp with time zone` | nullable; checked | Timestamp when membership ceased to be active. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `campaign_memberships_pkey` — `PRIMARY KEY (campaign_session_id, account_id)`
- `campaign_memberships_account_id_fkey` — `FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `campaign_memberships_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_memberships_inviter_account_id_fkey` — `FOREIGN KEY (inviter_account_id) REFERENCES accounts(id) ON DELETE SET NULL`
- `campaign_memberships_check` — `CHECK ((state = ANY (ARRAY['invited'::text, 'active'::text])) AND left_at IS NULL OR (state = ANY (ARRAY['left'::text, 'removed'::text])) AND left_at IS NOT NULL)`
- `campaign_memberships_check1` — `CHECK (role <> 'game_master'::text OR state <> 'invited'::text)`
- `campaign_memberships_check2` — `CHECK (state = 'active'::text AND accepted_at IS NOT NULL OR state <> 'active'::text AND accepted_at IS NULL)`
- `campaign_memberships_role_check` — `CHECK (role = ANY (ARRAY['game_master'::text, 'player'::text]))`
- `campaign_memberships_state_check` — `CHECK (state = ANY (ARRAY['invited'::text, 'active'::text, 'left'::text, 'removed'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_memberships_account_idx` — `CREATE INDEX campaign_memberships_account_idx ON public.campaign_memberships USING btree (account_id, campaign_session_id)`
- `campaign_memberships_one_gm_idx` — `CREATE UNIQUE INDEX campaign_memberships_one_gm_idx ON public.campaign_memberships USING btree (campaign_session_id) WHERE ((role = 'game_master'::text) AND (state = 'active'::text))`

</details>

### `campaign_sessions`

**Purpose.** Primary campaign aggregate row. `payload_json` contains the versioned game/session aggregate while dedicated columns carry ownership, lifecycle, policy, retention, and theme metadata used for indexed authorization and lifecycle checks.

**Access pattern.** This is a central hot row. Turn commits select it `FOR UPDATE`, verify the expected `revision`, atomically write aggregate/character changes, and append audits/receipts. Lifecycle and membership loaders partition by `owner_key`, `owner_account_id`, campaign `id`, or membership joins. New campaigns are inserted with their GM membership in one transaction. `owner_account_id` remains nullable for legacy/local rows, so it must not be treated as a universal hosted-owner predicate. The lifecycle hard-delete statement is blocked once `custom_action_point_ledger.campaign_id` references the campaign because that FK uses default `NO ACTION` rather than cascade.

**Migration source(s).** `migrations/0001_server_storage.sql`, `migrations/0007_campaign_content_pins.sql`, `migrations/0010_campaign_lifecycle.sql`, `migrations/0028_campaign_memberships.sql`, `migrations/0029_campaign_membership_theme.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application.rs:2631` (UPDATE), `crates/game-server/src/generation_ledger.rs:716` (INSERT), `crates/game-server/src/repository.rs:313` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/action_points.rs:331` (INSERT), `crates/game-server/src/repository/governance.rs:976` (INSERT), `crates/game-server/src/repository/inspiration.rs:2625` (INSERT/SELECT), `crates/game-server/src/repository/jobs.rs:564` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/legacy.rs:258` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:498` (DELETE/INSERT/SELECT/UPDATE), `crates/game-server/src/repository/memberships.rs:226` (INSERT/SELECT), `crates/game-server/src/repository/operations.rs:263` (SELECT), `crates/game-server/src/repository/pins.rs:45` (SELECT/UPDATE), `crates/game-server/src/repository/presentations.rs:1697` (INSERT), `crates/game-server/src/repository/recaps.rs:151` (DELETE/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK | Stable application-generated identifier for the `campaign_sessions` row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `revision` | `bigint` | required; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `payload_json` | `jsonb` | required | Versioned authoritative campaign/game aggregate serialized by `game-core`; turn commits replace it only after expected-revision validation. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |
| `content_pin_legacy_eligible` | `boolean` | required; default `false` | Boolean flag indicating whether content pin legacy eligible is true for this row. |
| `owner_key` | `text` | required; default `'local-owner'`; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `lifecycle_revision` | `bigint` | required; default `1`; checked | Monotonic campaign-lifecycle revision used for optimistic concurrency and audit ordering. |
| `lifecycle_state` | `text` | required; default `'active'`; checked | Campaign lifecycle state (`active`, `archived`, or deletion workflow state as constrained). |
| `archived_at` | `timestamp with time zone` | nullable; checked | Timestamp when the campaign entered archived state; null otherwise. |
| `safety_policy_id` | `text` | required; default `'safety:private-mvp:v1'`; checked | Identifier for the associated safety policy; used to scope, join, or correlate this row. |
| `progression_policy_id` | `text` | required; default `'progression:srd-5.1-mvp:v1'`; checked | Identifier for the associated progression policy; used to scope, join, or correlate this row. |
| `retention_class` | `text` | required; default `'campaign_lifetime'`; checked | Controlled retention class discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `retention_delete_after` | `timestamp with time zone` | nullable; checked | Earliest timestamp at which bounded-retention cleanup may delete the row. |
| `owner_account_id` | `text` | nullable; FK → accounts(id) ON DELETE CASCADE; checked | Account that owns the row; server-derived and used as the authorization partition. |
| `theme_id` | `text` | nullable; checked | Validated campaign theme/content-pack identifier used for eligibility and presentation. |

<details>
<summary>Exact table constraints</summary>

- `campaign_sessions_pkey` — `PRIMARY KEY (id)`
- `campaign_sessions_owner_account_id_fkey` — `FOREIGN KEY (owner_account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `campaign_sessions_lifecycle_revision_check` — `CHECK (lifecycle_revision > 0)`
- `campaign_sessions_lifecycle_shape` — `CHECK (lifecycle_state = 'active'::text AND archived_at IS NULL AND retention_class = 'campaign_lifetime'::text AND retention_delete_after IS NULL OR lifecycle_state = 'archived'::text AND archived_at IS NOT NULL AND retention_class = 'archived_owner_managed'::text AND retention_delete_after IS NULL)`
- `campaign_sessions_lifecycle_state_check` — `CHECK (lifecycle_state = ANY (ARRAY['active'::text, 'archived'::text]))`
- `campaign_sessions_owner_account_id_local_required` — `CHECK (owner_key <> 'local-owner'::text OR owner_account_id = 'account:local'::text)`
- `campaign_sessions_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_sessions_progression_policy_id_check` — `CHECK (octet_length(progression_policy_id) >= 1 AND octet_length(progression_policy_id) <= 128 AND progression_policy_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_sessions_retention_class_check` — `CHECK (retention_class = ANY (ARRAY['campaign_lifetime'::text, 'archived_owner_managed'::text]))`
- `campaign_sessions_revision_check` — `CHECK (revision > 0)`
- `campaign_sessions_safety_policy_id_check` — `CHECK (octet_length(safety_policy_id) >= 1 AND octet_length(safety_policy_id) <= 128 AND safety_policy_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_sessions_schema_version_check` — `CHECK (schema_version > 0)`
- `campaign_sessions_theme_id_check` — `CHECK (theme_id IS NULL OR (theme_id = ANY (ARRAY['dev.manchester-arcana.rainbound-borough'::text, 'dev.manchester-arcana.emberline-archive'::text])))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_sessions_owner_account_idx` — `CREATE INDEX campaign_sessions_owner_account_idx ON public.campaign_sessions USING btree (owner_account_id, updated_at DESC, id) WHERE (owner_account_id IS NOT NULL)`
- `campaign_sessions_owner_lifecycle_idx` — `CREATE INDEX campaign_sessions_owner_lifecycle_idx ON public.campaign_sessions USING btree (owner_key, lifecycle_state, updated_at DESC, id)`

</details>


## Campaign lifecycle and lobby

### `campaign_deletion_preparations`

**Purpose.** Short-lived, private canonical export created during the first phase of destructive campaign deletion.

**Access pattern.** Preparation inserts/loads by `(owner_key, campaign_session_id, deletion_id)`. Confirmation locks the unexpired row and verifies captured campaign/lifecycle revisions and digest before deletion. Expired preparations are removed in bounded `FOR UPDATE SKIP LOCKED` batches. Default lifetime is one hour.

**Migration source(s).** `migrations/0010_campaign_lifecycle.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/lifecycle.rs:4005` (DELETE/INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `owner_key` | `text` | required; PK component; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `campaign_session_id` | `text` | required; PK component; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `deletion_id` | `text` | required; PK component; checked | Identifier for the associated deletion; used to scope, join, or correlate this row. |
| `campaign_revision` | `bigint` | required; checked | Campaign aggregate revision captured or produced by this operation. |
| `lifecycle_revision` | `bigint` | required; checked | Monotonic campaign-lifecycle revision used for optimistic concurrency and audit ordering. |
| `canonical_export_digest` | `text` | required; checked | Deterministic digest of canonical export, used for integrity/equality checks without retaining the raw input. |
| `canonical_export_json` | `text` | required; checked | Private canonical campaign export held only during the one-hour deletion-confirmation window. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `expires_at` | `timestamp with time zone` | required; default `(CURRENT_TIMESTAMP + '01:00:00'::interval)` | Deadline after which the row/workflow is no longer valid. |

<details>
<summary>Exact table constraints</summary>

- `campaign_deletion_preparations_pkey` — `PRIMARY KEY (owner_key, campaign_session_id, deletion_id)`
- `campaign_deletion_preparations_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_deletion_preparations_campaign_revision_check` — `CHECK (campaign_revision > 0)`
- `campaign_deletion_preparations_canonical_export_digest_check` — `CHECK (canonical_export_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_deletion_preparations_canonical_export_json_check` — `CHECK (octet_length(canonical_export_json) >= 2 AND octet_length(canonical_export_json) <= 2097152 AND jsonb_typeof(canonical_export_json::jsonb) = 'object'::text)`
- `campaign_deletion_preparations_deletion_id_check` — `CHECK (octet_length(deletion_id) >= 1 AND octet_length(deletion_id) <= 128 AND deletion_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_deletion_preparations_lifecycle_revision_check` — `CHECK (lifecycle_revision > 0)`
- `campaign_deletion_preparations_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_deletion_preparations_expiry_idx` — `CREATE INDEX campaign_deletion_preparations_expiry_idx ON public.campaign_deletion_preparations USING btree (expires_at)`

</details>

### `campaign_deletion_tombstones`

**Purpose.** Minimal post-deletion marker used to suppress immediate campaign-ID reuse/replay without retaining the deleted campaign export.

**Access pattern.** The confirmed-deletion transaction inserts the digest/revision marker before attempting the campaign delete. Lookups use owner plus campaign ID and require unexpired retention. Cleanup deletes expired rows in bounded locked batches; default retention is 35 days. If a `custom_action_point_ledger` FK blocks the campaign delete, the transaction rolls back this tombstone insert as well, so no false deletion marker is committed.

**Migration source(s).** `migrations/0010_campaign_lifecycle.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/lifecycle.rs:546` (DELETE/INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `owner_key` | `text` | required; PK component; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `campaign_session_id` | `text` | required; PK component; checked | Campaign that owns/scopes the row and is the principal partition key for access. |
| `deletion_id` | `text` | required; PK component; unique; checked | Identifier for the associated deletion; used to scope, join, or correlate this row. |
| `deleted_lifecycle_revision` | `bigint` | required; checked | Captured or expected revision for deleted lifecycle, used to reject stale operations. |
| `canonical_export_digest` | `text` | required; checked | Deterministic digest of canonical export, used for integrity/equality checks without retaining the raw input. |
| `deleted_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Timestamp when deleted occurred or becomes effective. |
| `retention_delete_after` | `timestamp with time zone` | required; default `(CURRENT_TIMESTAMP + '35 days'::interval)` | Earliest timestamp at which bounded-retention cleanup may delete the row. |

<details>
<summary>Exact table constraints</summary>

- `campaign_deletion_tombstones_pkey` — `PRIMARY KEY (owner_key, campaign_session_id, deletion_id)`
- `campaign_deletion_tombstones_deletion_id_key` — `UNIQUE (deletion_id)`
- `campaign_deletion_tombstones_campaign_session_id_check` — `CHECK (octet_length(campaign_session_id) >= 1 AND octet_length(campaign_session_id) <= 128 AND campaign_session_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_deletion_tombstones_canonical_export_digest_check` — `CHECK (canonical_export_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_deletion_tombstones_deleted_lifecycle_revision_check` — `CHECK (deleted_lifecycle_revision > 1)`
- `campaign_deletion_tombstones_deletion_id_check` — `CHECK (octet_length(deletion_id) >= 1 AND octet_length(deletion_id) <= 128 AND deletion_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_deletion_tombstones_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_deletion_tombstones_deletion_id_key` — `CREATE UNIQUE INDEX campaign_deletion_tombstones_deletion_id_key ON public.campaign_deletion_tombstones USING btree (deletion_id)`
- `campaign_deletion_tombstones_retention_idx` — `CREATE INDEX campaign_deletion_tombstones_retention_idx ON public.campaign_deletion_tombstones USING btree (retention_delete_after)`

</details>

### `campaign_lifecycle_audits`

**Purpose.** Append-only record of campaign lifecycle transitions such as play start/end, archive, restore, deletion preparation, and import.

**Access pattern.** Lifecycle transactions append a row at the resulting lifecycle revision. Export/history and operational metrics read by campaign and chronological order; rows are never updated. Campaign deletion cascades them after the minimal tombstone has been written.

**Migration source(s).** `migrations/0010_campaign_lifecycle.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/lifecycle.rs:1219` (INSERT/SELECT), `crates/game-server/src/repository/operations.rs:407` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique; checked | Stable application-generated identifier for the `campaign_lifecycle_audits` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `owner_key` | `text` | required; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `lifecycle_revision` | `bigint` | required; composite unique; checked | Monotonic campaign-lifecycle revision used for optimistic concurrency and audit ordering. |
| `event_kind` | `text` | required; checked | Controlled event discriminator used for audit interpretation and metrics. |
| `payload_json` | `jsonb` | required; checked | Typed details of the lifecycle transition at `lifecycle_revision`. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `campaign_lifecycle_audits_pkey` — `PRIMARY KEY (id)`
- `campaign_lifecycle_audits_campaign_session_id_lifecycle_rev_key` — `UNIQUE (campaign_session_id, lifecycle_revision)`
- `campaign_lifecycle_audits_id_campaign_session_id_key` — `UNIQUE (id, campaign_session_id)`
- `campaign_lifecycle_audits_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_lifecycle_audits_event_kind_check` — `CHECK (event_kind = ANY (ARRAY['play_started'::text, 'play_ended'::text, 'archived'::text, 'restored'::text, 'restore_imported'::text]))`
- `campaign_lifecycle_audits_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_lifecycle_audits_lifecycle_revision_check` — `CHECK (lifecycle_revision > 1)`
- `campaign_lifecycle_audits_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_lifecycle_audits_payload_json_check` — `CHECK (jsonb_typeof(payload_json) = 'object'::text AND octet_length(payload_json::text) >= 2 AND octet_length(payload_json::text) <= 16384)`
- `campaign_lifecycle_audits_schema_version_check` — `CHECK (schema_version = 1)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_lifecycle_audits_campaign_session_id_lifecycle_rev_key` — `CREATE UNIQUE INDEX campaign_lifecycle_audits_campaign_session_id_lifecycle_rev_key ON public.campaign_lifecycle_audits USING btree (campaign_session_id, lifecycle_revision)`
- `campaign_lifecycle_audits_history_idx` — `CREATE INDEX campaign_lifecycle_audits_history_idx ON public.campaign_lifecycle_audits USING btree (campaign_session_id, lifecycle_revision, id)`
- `campaign_lifecycle_audits_id_campaign_session_id_key` — `CREATE UNIQUE INDEX campaign_lifecycle_audits_id_campaign_session_id_key ON public.campaign_lifecycle_audits USING btree (id, campaign_session_id)`

</details>

### `campaign_lifecycle_receipts`

**Purpose.** Time-bounded idempotency receipts for lifecycle commands.

**Access pattern.** Lifecycle commands probe by `(owner_key, campaign_session_id, idempotency_key)`, verify the fingerprint and expected lifecycle revision, and store the response. The table deliberately has no campaign FK so delete-command replay survives campaign deletion. Cleanup uses bounded `FOR UPDATE SKIP LOCKED` batches after the 30-day retention deadline.

**Migration source(s).** `migrations/0010_campaign_lifecycle.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application/lifecycle.rs:453` (SELECT), `crates/game-server/src/repository/lifecycle.rs:591` (DELETE/INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `owner_key` | `text` | required; PK component; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `campaign_session_id` | `text` | required; PK component; checked | Campaign that owns/scopes the row and is the principal partition key for access. |
| `idempotency_key` | `text` | required; PK component; checked | Opaque client/operator retry key within the table’s documented scope. |
| `command_kind` | `text` | required; checked | Controlled command discriminator used during idempotent replay validation. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `expected_lifecycle_revision` | `bigint` | required; checked | Lifecycle revision the caller expected before the command. |
| `result_lifecycle_revision` | `bigint` | required; checked | Lifecycle revision produced by the command and replayed from the receipt. |
| `response_json` | `text` | required; checked | Bounded serialized response replayed for an exact duplicate command. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `retention_delete_after` | `timestamp with time zone` | required; default `(CURRENT_TIMESTAMP + '30 days'::interval)` | Earliest timestamp at which bounded-retention cleanup may delete the row. |

<details>
<summary>Exact table constraints</summary>

- `campaign_lifecycle_receipts_pkey` — `PRIMARY KEY (owner_key, campaign_session_id, idempotency_key)`
- `campaign_lifecycle_receipts_campaign_session_id_check` — `CHECK (octet_length(campaign_session_id) >= 1 AND octet_length(campaign_session_id) <= 128 AND campaign_session_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_lifecycle_receipts_check` — `CHECK (result_lifecycle_revision = (expected_lifecycle_revision + 1))`
- `campaign_lifecycle_receipts_command_kind_check` — `CHECK (command_kind = ANY (ARRAY['play_start'::text, 'play_end'::text, 'archive'::text, 'restore_archive'::text, 'delete'::text, 'restore_export'::text]))`
- `campaign_lifecycle_receipts_expected_lifecycle_revision_check` — `CHECK (expected_lifecycle_revision >= 0)`
- `campaign_lifecycle_receipts_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_lifecycle_receipts_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_lifecycle_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_lifecycle_receipts_response_json_check` — `CHECK (octet_length(response_json) >= 1 AND octet_length(response_json) <= 65536 AND jsonb_typeof(response_json::jsonb) IS NOT NULL)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_lifecycle_receipts_retention_idx` — `CREATE INDEX campaign_lifecycle_receipts_retention_idx ON public.campaign_lifecycle_receipts USING btree (retention_delete_after)`

</details>

### `campaign_play_session_participants`

**Purpose.** Lobby roster for one play session, including each account’s selected runtime character, readiness state, and handoff revision.

**Access pattern.** **Schema-ready, not yet used by production Rust SQL.** Intended access is by `(play_session_id, account_id)`, with roster scans by play session and account-oriented lookup via the supporting index. Rows cascade when the play session or account is deleted.

**Migration source(s).** `migrations/0030_campaign_lobbies_and_turns.sql`

**SQL references (runtime, maintenance, and tests).** None outside migrations.

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `play_session_id` | `text` | required; PK component; FK → campaign_play_sessions(id) ON DELETE CASCADE | Play-session/lobby that scopes the row. |
| `account_id` | `text` | required; PK component; FK → accounts(id) ON DELETE CASCADE | Account participating in or owning the scoped relation. |
| `runtime_character_id` | `text` | nullable; checked | Identifier for the associated runtime character; used to scope, join, or correlate this row. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `ready_at` | `timestamp with time zone` | nullable; checked | Timestamp when ready occurred or becomes effective. |
| `handoff_revision` | `bigint` | required; default `0`; checked | Participant handoff/readiness revision used to reject stale lobby changes. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `campaign_play_session_participants_pkey` — `PRIMARY KEY (play_session_id, account_id)`
- `campaign_play_session_participants_account_id_fkey` — `FOREIGN KEY (account_id) REFERENCES accounts(id) ON DELETE CASCADE`
- `campaign_play_session_participants_play_session_id_fkey` — `FOREIGN KEY (play_session_id) REFERENCES campaign_play_sessions(id) ON DELETE CASCADE`
- `campaign_play_session_participants_check` — `CHECK ((state = ANY (ARRAY['not_ready'::text, 'left'::text])) AND ready_at IS NULL OR (state = ANY (ARRAY['ready'::text, 'human_active'::text, 'ai_substitute'::text])) AND ready_at IS NOT NULL)`
- `campaign_play_session_participants_handoff_revision_check` — `CHECK (handoff_revision >= 0)`
- `campaign_play_session_participants_runtime_character_id_check` — `CHECK (runtime_character_id IS NULL OR octet_length(runtime_character_id) >= 1 AND octet_length(runtime_character_id) <= 128 AND runtime_character_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_play_session_participants_state_check` — `CHECK (state = ANY (ARRAY['not_ready'::text, 'ready'::text, 'human_active'::text, 'ai_substitute'::text, 'left'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_play_session_participants_account_idx` — `CREATE INDEX campaign_play_session_participants_account_idx ON public.campaign_play_session_participants USING btree (account_id, play_session_id)`
- `campaign_play_session_participants_session_idx` — `CREATE INDEX campaign_play_session_participants_session_idx ON public.campaign_play_session_participants USING btree (play_session_id, state, account_id)`

</details>

### `campaign_play_sessions`

**Purpose.** A bounded opening/closing interval for campaign play. Migration 0030 extends the original lifecycle session with lobby GM, start policy, membership snapshot, and active-turn revision metadata.

**Access pattern.** Lifecycle code lists by campaign/owner, probes for one open (`waiting` or `active`) row under lock, inserts a waiting session, and closes it atomically. Membership summaries left-join the open row. The newer lobby columns currently rely on defaults/schema constraints; participant/turn-control repository writes are not yet implemented.

**Migration source(s).** `migrations/0010_campaign_lifecycle.sql`, `migrations/0030_campaign_lobbies_and_turns.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/lifecycle.rs:499` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/memberships.rs:541` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique; checked | Stable application-generated identifier for the `campaign_play_sessions` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `owner_key` | `text` | required; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `started_campaign_revision` | `bigint` | required; checked | Captured or expected revision for started campaign, used to reject stale operations. |
| `ended_campaign_revision` | `bigint` | nullable; checked | Captured or expected revision for ended campaign, used to reject stale operations. |
| `opened_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Timestamp when the play session/lobby was opened. |
| `closed_at` | `timestamp with time zone` | nullable | Timestamp when the play session was closed; null while open. |
| `close_reason` | `text` | nullable; checked | Controlled explanation for why the play session closed. |
| `gm_account_id` | `text` | nullable; FK → accounts(id) ON DELETE SET NULL; checked | Identifier for the associated gm account; used to scope, join, or correlate this row. |
| `start_policy` | `text` | required; default `'wait_for_all'`; checked | Controlled start policy discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `expected_membership_revision` | `bigint` | required; default `0`; checked | Captured or expected revision for expected membership, used to reject stale operations. |
| `active_turn_revision` | `bigint` | required; default `0`; checked | Current optimistic revision for lobby/turn-control changes in this play session. |

<details>
<summary>Exact table constraints</summary>

- `campaign_play_sessions_pkey` — `PRIMARY KEY (id)`
- `campaign_play_sessions_id_campaign_session_id_key` — `UNIQUE (id, campaign_session_id)`
- `campaign_play_sessions_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_play_sessions_gm_account_id_fkey` — `FOREIGN KEY (gm_account_id) REFERENCES accounts(id) ON DELETE SET NULL`
- `campaign_play_sessions_active_turn_revision_check` — `CHECK (active_turn_revision >= 0)`
- `campaign_play_sessions_close_reason_check` — `CHECK (close_reason IS NULL OR (close_reason = ANY (ARRAY['owner_ended'::text, 'archive'::text, 'restore_import'::text])))`
- `campaign_play_sessions_ended_campaign_revision_check` — `CHECK (ended_campaign_revision > 0)`
- `campaign_play_sessions_expected_membership_revision_check` — `CHECK (expected_membership_revision >= 0)`
- `campaign_play_sessions_gm_required_for_new` — `CHECK (state = 'closed'::text OR state = 'waiting'::text OR gm_account_id IS NOT NULL)`
- `campaign_play_sessions_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_play_sessions_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_play_sessions_schema_version_check` — `CHECK (schema_version = 1)`
- `campaign_play_sessions_start_policy_check` — `CHECK (start_policy = ANY (ARRAY['wait_for_all'::text, 'start_with_ai_substitutes'::text]))`
- `campaign_play_sessions_started_campaign_revision_check` — `CHECK (started_campaign_revision > 0)`
- `campaign_play_sessions_state_check` — `CHECK (state = ANY (ARRAY['waiting'::text, 'active'::text, 'closed'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_play_sessions_gm_idx` — `CREATE INDEX campaign_play_sessions_gm_idx ON public.campaign_play_sessions USING btree (gm_account_id, state, opened_at DESC, id) WHERE (gm_account_id IS NOT NULL)`
- `campaign_play_sessions_history_idx` — `CREATE INDEX campaign_play_sessions_history_idx ON public.campaign_play_sessions USING btree (campaign_session_id, opened_at DESC, id)`
- `campaign_play_sessions_id_campaign_session_id_key` — `CREATE UNIQUE INDEX campaign_play_sessions_id_campaign_session_id_key ON public.campaign_play_sessions USING btree (id, campaign_session_id)`
- `campaign_play_sessions_one_open_idx` — `CREATE UNIQUE INDEX campaign_play_sessions_one_open_idx ON public.campaign_play_sessions USING btree (campaign_session_id) WHERE (state = ANY (ARRAY['waiting'::text, 'active'::text]))`

</details>

### `campaign_private_recaps`

**Purpose.** Owner-private, immutable recap generated only from committed turn-audit facts.

**Access pattern.** Generation first verifies campaign ownership/revision, reads a turn-audit range, derives a minimized body, and inserts an idempotent row. Latest reads use owner/campaign and descending revision/time indexes. Rows cascade with the campaign and are never updated.

**Migration source(s).** `migrations/0023_private_campaign_recaps.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/lifecycle.rs:1559` (INSERT/SELECT), `crates/game-server/src/repository/recaps.rs:222` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique; checked | Stable application-generated identifier for the `campaign_private_recaps` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `owner_key` | `text` | required; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `campaign_revision` | `bigint` | required; composite unique; checked | Campaign aggregate revision captured or produced by this operation. |
| `idempotency_key` | `text` | required; composite unique; checked | Opaque client/operator retry key within the table’s documented scope. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `first_turn_number` | `bigint` | nullable; checked | First included turn number; null when the source range is empty. |
| `last_turn_number` | `bigint` | nullable; checked | Last included turn number; null when the source range is empty. |
| `source_audit_count` | `bigint` | required; checked | Number of committed audit rows included in the derivation. |
| `source_audit_digest` | `text` | required; checked | Digest of the exact ordered turn-audit inputs used to derive the recap. |
| `template_id` | `text` | required; checked | Identifier for the associated template; used to scope, join, or correlate this row. |
| `body` | `text` | required; checked | Owner-private recap text derived from committed audits; never provider/source raw text. |
| `body_digest` | `text` | required; checked | Digest of the stored body used to verify immutable recap/presentation integrity. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `campaign_private_recaps_pkey` — `PRIMARY KEY (id)`
- `campaign_private_recaps_campaign_session_id_campaign_revisi_key` — `UNIQUE (campaign_session_id, campaign_revision)`
- `campaign_private_recaps_campaign_session_id_idempotency_key_key` — `UNIQUE (campaign_session_id, idempotency_key)`
- `campaign_private_recaps_id_campaign_session_id_key` — `UNIQUE (id, campaign_session_id)`
- `campaign_private_recaps_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_private_recaps_body_check` — `CHECK (octet_length(body) >= 1 AND octet_length(body) <= 131072)`
- `campaign_private_recaps_body_digest_check` — `CHECK (body_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_private_recaps_campaign_revision_check` — `CHECK (campaign_revision > 0)`
- `campaign_private_recaps_check` — `CHECK (source_audit_count = 0 AND first_turn_number IS NULL AND last_turn_number IS NULL OR source_audit_count > 0 AND first_turn_number IS NOT NULL AND last_turn_number IS NOT NULL AND last_turn_number >= first_turn_number)`
- `campaign_private_recaps_first_turn_number_check` — `CHECK (first_turn_number > 0)`
- `campaign_private_recaps_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_private_recaps_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_private_recaps_last_turn_number_check` — `CHECK (last_turn_number > 0)`
- `campaign_private_recaps_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_private_recaps_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_private_recaps_schema_version_check` — `CHECK (schema_version = 1)`
- `campaign_private_recaps_source_audit_count_check` — `CHECK (source_audit_count >= 0)`
- `campaign_private_recaps_source_audit_digest_check` — `CHECK (source_audit_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_private_recaps_template_id_check` — `CHECK (template_id = 'private-recap-v1'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_private_recaps_campaign_session_id_campaign_revisi_key` — `CREATE UNIQUE INDEX campaign_private_recaps_campaign_session_id_campaign_revisi_key ON public.campaign_private_recaps USING btree (campaign_session_id, campaign_revision)`
- `campaign_private_recaps_campaign_session_id_idempotency_key_key` — `CREATE UNIQUE INDEX campaign_private_recaps_campaign_session_id_idempotency_key_key ON public.campaign_private_recaps USING btree (campaign_session_id, idempotency_key)`
- `campaign_private_recaps_id_campaign_session_id_key` — `CREATE UNIQUE INDEX campaign_private_recaps_id_campaign_session_id_key ON public.campaign_private_recaps USING btree (id, campaign_session_id)`
- `campaign_private_recaps_owner_idx` — `CREATE INDEX campaign_private_recaps_owner_idx ON public.campaign_private_recaps USING btree (owner_key, campaign_session_id, campaign_revision DESC)`

</details>

### `campaign_turn_states`

**Purpose.** Single mutable turn-control snapshot per play session: phase, active actor/character, round/turn counters, revision, and bounded auxiliary JSON.

**Access pattern.** **Schema-ready, not yet used by production Rust SQL.** Intended access is a primary-key load/lock by `play_session_id`, optimistic update by `revision`, and active-character lookup through the partial index. It is distinct from immutable `turn_audits`.

**Migration source(s).** `migrations/0030_campaign_lobbies_and_turns.sql`

**SQL references (runtime, maintenance, and tests).** None outside migrations.

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `play_session_id` | `text` | required; PK; composite unique; FK → campaign_play_sessions(id) ON DELETE CASCADE; checked | Play-session/lobby that scopes the row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `phase` | `text` | required; checked | Current lobby/turn-control phase. |
| `active_account_id` | `text` | nullable; FK → accounts(id) ON DELETE SET NULL | Identifier for the associated active account; used to scope, join, or correlate this row. |
| `active_character_id` | `text` | nullable; checked | Identifier for the associated active character; used to scope, join, or correlate this row. |
| `round` | `bigint` | required; checked | Current bounded round counter in turn control. |
| `turn_number` | `bigint` | required; checked | Campaign/play-session turn ordinal used for ordering and cooldown/history queries. |
| `revision` | `bigint` | required; composite unique; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `bounded_json` | `jsonb` | required; checked | Size-constrained auxiliary turn-control data; core phase/actor/revision fields remain relational. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `campaign_turn_states_pkey` — `PRIMARY KEY (play_session_id)`
- `campaign_turn_states_play_session_id_revision_key` — `UNIQUE (play_session_id, revision)`
- `campaign_turn_states_active_account_id_fkey` — `FOREIGN KEY (active_account_id) REFERENCES accounts(id) ON DELETE SET NULL`
- `campaign_turn_states_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_turn_states_play_session_id_fkey` — `FOREIGN KEY (play_session_id) REFERENCES campaign_play_sessions(id) ON DELETE CASCADE`
- `campaign_turn_states_active_character_id_check` — `CHECK (active_character_id IS NULL OR octet_length(active_character_id) >= 1 AND octet_length(active_character_id) <= 128 AND active_character_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `campaign_turn_states_bounded_json_check` — `CHECK (jsonb_typeof(bounded_json) = 'object'::text AND octet_length(bounded_json::text) >= 2 AND octet_length(bounded_json::text) <= 65536)`
- `campaign_turn_states_phase_check` — `CHECK (phase = ANY (ARRAY['game_master_generation'::text, 'player_action'::text, 'resolving'::text, 'completed'::text]))`
- `campaign_turn_states_play_session_id_check` — `CHECK (play_session_id <> ''::text)`
- `campaign_turn_states_revision_check` — `CHECK (revision > 0)`
- `campaign_turn_states_round_check` — `CHECK (round > 0)`
- `campaign_turn_states_turn_number_check` — `CHECK (turn_number > 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `campaign_turn_states_active_character_idx` — `CREATE INDEX campaign_turn_states_active_character_idx ON public.campaign_turn_states USING btree (active_account_id, active_character_id, play_session_id) WHERE (active_account_id IS NOT NULL)`
- `campaign_turn_states_play_session_id_revision_key` — `CREATE UNIQUE INDEX campaign_turn_states_play_session_id_revision_key ON public.campaign_turn_states USING btree (play_session_id, revision)`

</details>

### `lobby_command_receipts`

**Purpose.** Idempotency receipts for lobby and turn-control commands scoped to a play session.

**Access pattern.** **Schema-ready, not yet used by production Rust SQL.** Intended access is an exact probe/insert on `(play_session_id, idempotency_key)`, with fingerprint comparison and optimistic revision results. The row cascades with its play session.

**Migration source(s).** `migrations/0030_campaign_lobbies_and_turns.sql`

**SQL references (runtime, maintenance, and tests).** None outside migrations.

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `play_session_id` | `text` | required; PK component; FK → campaign_play_sessions(id) ON DELETE CASCADE | Play-session/lobby that scopes the row. |
| `idempotency_key` | `text` | required; PK component; checked | Opaque client/operator retry key within the table’s documented scope. |
| `command_kind` | `text` | required; checked | Controlled command discriminator used during idempotent replay validation. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `expected_revision` | `bigint` | required; checked | Caller-expected revision checked before applying the command. |
| `result_revision` | `bigint` | required; checked | Revision returned after the idempotent command completed. |
| `response_json` | `text` | required; checked | Bounded serialized response replayed for an exact duplicate command. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `lobby_command_receipts_pkey` — `PRIMARY KEY (play_session_id, idempotency_key)`
- `lobby_command_receipts_play_session_id_fkey` — `FOREIGN KEY (play_session_id) REFERENCES campaign_play_sessions(id) ON DELETE CASCADE`
- `lobby_command_receipts_command_kind_check` — `CHECK (command_kind = ANY (ARRAY['lobby_start'::text, 'lobby_end'::text]))`
- `lobby_command_receipts_expected_revision_check` — `CHECK (expected_revision >= 0)`
- `lobby_command_receipts_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `lobby_command_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `lobby_command_receipts_response_json_check` — `CHECK (octet_length(response_json) >= 1 AND octet_length(response_json) <= 65536 AND jsonb_typeof(response_json::jsonb) IS NOT NULL)`
- `lobby_command_receipts_result_revision_check` — `CHECK (result_revision > 0)`

</details>

### `turn_control_audits`

**Purpose.** Append-only audit stream for lobby/turn-control transitions rather than game-world turn events.

**Access pattern.** **Schema-ready, not yet used by production Rust SQL.** Intended writes accompany turn-state transitions; history reads use `(play_session_id, created_at)` and actor investigations use the actor index. Account deletion sets the actor to null while preserving the event.

**Migration source(s).** `migrations/0030_campaign_lobbies_and_turns.sql`

**SQL references (runtime, maintenance, and tests).** None outside migrations.

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `turn_control_audits` row. |
| `play_session_id` | `text` | required; FK → campaign_play_sessions(id) ON DELETE CASCADE | Play-session/lobby that scopes the row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `event_kind` | `text` | required; checked | Controlled event discriminator used for audit interpretation and metrics. |
| `actor_account_id` | `text` | nullable; FK → accounts(id) ON DELETE SET NULL | Identifier for the associated actor account; used to scope, join, or correlate this row. |
| `from_phase` | `text` | nullable; checked | Turn-control phase before the audited transition; null when the event has no prior phase. |
| `to_phase` | `text` | nullable; checked | Turn-control phase after the audited transition; null when the event has no resulting phase. |
| `from_revision` | `bigint` | nullable; checked | Captured or expected revision for from, used to reject stale operations. |
| `to_revision` | `bigint` | nullable; checked | Captured or expected revision for to, used to reject stale operations. |
| `payload_json` | `jsonb` | required; checked | Bounded transition-specific metadata for the turn-control event. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `turn_control_audits_pkey` — `PRIMARY KEY (id)`
- `turn_control_audits_actor_account_id_fkey` — `FOREIGN KEY (actor_account_id) REFERENCES accounts(id) ON DELETE SET NULL`
- `turn_control_audits_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `turn_control_audits_play_session_id_fkey` — `FOREIGN KEY (play_session_id) REFERENCES campaign_play_sessions(id) ON DELETE CASCADE`
- `turn_control_audits_event_kind_check` — `CHECK (event_kind = ANY (ARRAY['lobby_created'::text, 'member_readied'::text, 'member_unreadied'::text, 'member_left'::text, 'lobby_started'::text, 'lobby_started_replay'::text, 'lobby_ended'::text, 'turn_boundary'::text, 'handoff'::text]))`
- `turn_control_audits_from_phase_check` — `CHECK (from_phase IS NULL OR (from_phase = ANY (ARRAY['game_master_generation'::text, 'player_action'::text, 'resolving'::text, 'completed'::text])))`
- `turn_control_audits_from_revision_check` — `CHECK (from_revision IS NULL OR from_revision >= 0)`
- `turn_control_audits_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `turn_control_audits_payload_json_check` — `CHECK (jsonb_typeof(payload_json) = 'object'::text AND octet_length(payload_json::text) >= 2 AND octet_length(payload_json::text) <= 16384)`
- `turn_control_audits_to_phase_check` — `CHECK (to_phase IS NULL OR (to_phase = ANY (ARRAY['game_master_generation'::text, 'player_action'::text, 'resolving'::text, 'completed'::text])))`
- `turn_control_audits_to_revision_check` — `CHECK (to_revision IS NULL OR to_revision >= 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `turn_control_audits_actor_idx` — `CREATE INDEX turn_control_audits_actor_idx ON public.turn_control_audits USING btree (actor_account_id, created_at DESC, id) WHERE (actor_account_id IS NOT NULL)`
- `turn_control_audits_session_idx` — `CREATE INDEX turn_control_audits_session_idx ON public.turn_control_audits USING btree (campaign_session_id, play_session_id, created_at DESC, id)`

</details>


## Core campaign state and turns

### `characters`

**Purpose.** Generic campaign character aggregate used by the original atomic turn repository and legacy import. This is separate from account library `player_characters` and typed runtime `hero_characters`.

**Access pattern.** Creation inserts campaign-associated snapshots. Turn commit loads each changed row `FOR UPDATE`, verifies revision and campaign ownership, then updates JSON and revision in the same transaction as `campaign_sessions`, `turn_audits`, and `command_receipts`. Final FK deletion is cascade.

**Migration source(s).** `migrations/0001_server_storage.sql`, `migrations/0010_campaign_lifecycle.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository.rs:334` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/inspiration.rs:2728` (INSERT/SELECT), `crates/game-server/src/repository/legacy.rs:280` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1522` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK | Stable application-generated identifier for the `characters` row. |
| `campaign_session_id` | `text` | nullable; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `revision` | `bigint` | required; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `payload_json` | `jsonb` | required | Versioned serialized generic character aggregate used by the atomic turn repository and legacy import. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `characters_pkey` — `PRIMARY KEY (id)`
- `characters_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `characters_revision_check` — `CHECK (revision > 0)`
- `characters_schema_version_check` — `CHECK (schema_version > 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `characters_campaign_session_idx` — `CREATE INDEX characters_campaign_session_idx ON public.characters USING btree (campaign_session_id)`

</details>

### `command_receipts`

**Purpose.** Idempotency receipt for an atomic campaign command/turn commit.

**Access pattern.** Before mutation, the repository probes `(campaign_session_id, idempotency_key)` and compares the request fingerprint. Successful commit inserts the resulting revision, audit ID, and bounded serialized response in the same transaction. Rows are immutable and cascade with the campaign/turn audit.

**Migration source(s).** `migrations/0002_command_receipts.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository.rs:409` (INSERT/SELECT), `crates/game-server/src/repository/legacy.rs:320` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1604` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `idempotency_key` | `text` | required; PK component | Opaque client/operator retry key within the table’s documented scope. |
| `command_kind` | `text` | required | Controlled command discriminator used during idempotent replay validation. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `expected_revision` | `bigint` | required; checked | Caller-expected revision checked before applying the command. |
| `result_revision` | `bigint` | required; checked | Revision returned after the idempotent command completed. |
| `audit_id` | `text` | required; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated audit; used to scope, join, or correlate this row. |
| `response_json` | `text` | required; checked | Bounded serialized response replayed for an exact duplicate command. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `command_receipts_pkey` — `PRIMARY KEY (campaign_session_id, idempotency_key)`
- `command_receipts_audit_id_campaign_session_id_fkey` — `FOREIGN KEY (audit_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `command_receipts_check` — `CHECK (result_revision = (expected_revision + 1))`
- `command_receipts_expected_revision_check` — `CHECK (expected_revision > 0)`
- `command_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `command_receipts_response_json_check` — `CHECK (octet_length(response_json) >= 1 AND octet_length(response_json) <= 65536 AND jsonb_typeof(response_json::jsonb) IS NOT NULL)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `command_receipts_audit_idx` — `CREATE INDEX command_receipts_audit_idx ON public.command_receipts USING btree (audit_id, campaign_session_id)`

</details>

### `turn_audits`

**Purpose.** Immutable authoritative event/audit record for a committed game turn.

**Access pattern.** Atomic turn commit inserts exactly one row after locking aggregates. History, recaps, generation origin checks, presentation publication, inspiration selection, and operations metrics read by campaign, turn number, ID, or correlation ID. Composite uniqueness/FKs make an origin turn inseparable from its campaign.

**Migration source(s).** `migrations/0001_server_storage.sql`, `migrations/0003_turn_audit_correlation.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/generation_ledger.rs:759` (INSERT), `crates/game-server/src/repository.rs:880` (INSERT/SELECT), `crates/game-server/src/repository/inspiration.rs:2657` (INSERT/SELECT), `crates/game-server/src/repository/jobs.rs:592` (INSERT/SELECT), `crates/game-server/src/repository/legacy.rs:300` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:630` (INSERT/SELECT), `crates/game-server/src/repository/operations.rs:382` (SELECT), `crates/game-server/src/repository/presentations.rs:311` (INSERT/SELECT), `crates/game-server/src/repository/recaps.rs:199` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique | Stable application-generated identifier for the `turn_audits` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `turn_number` | `bigint` | required; composite unique; checked | Campaign/play-session turn ordinal used for ordering and cooldown/history queries. |
| `actor_id` | `text` | nullable | Identifier for the associated actor; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `payload_json` | `jsonb` | required | Committed immutable turn/event envelope used as generation, recap, and history authority. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `correlation_id` | `text` | nullable; checked | Request/trace correlation identifier used to connect audits and operational logs. |

<details>
<summary>Exact table constraints</summary>

- `turn_audits_pkey` — `PRIMARY KEY (id)`
- `turn_audits_campaign_session_id_turn_number_key` — `UNIQUE (campaign_session_id, turn_number)`
- `turn_audits_id_campaign_session_id_key` — `UNIQUE (id, campaign_session_id)`
- `turn_audits_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `turn_audits_correlation_id_check` — `CHECK (correlation_id IS NULL OR octet_length(correlation_id) >= 1 AND octet_length(correlation_id) <= 128 AND correlation_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `turn_audits_schema_version_check` — `CHECK (schema_version > 0)`
- `turn_audits_turn_number_check` — `CHECK (turn_number >= 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `turn_audits_campaign_session_id_turn_number_key` — `CREATE UNIQUE INDEX turn_audits_campaign_session_id_turn_number_key ON public.turn_audits USING btree (campaign_session_id, turn_number)`
- `turn_audits_campaign_session_idx` — `CREATE INDEX turn_audits_campaign_session_idx ON public.turn_audits USING btree (campaign_session_id, turn_number)`
- `turn_audits_correlation_idx` — `CREATE INDEX turn_audits_correlation_idx ON public.turn_audits USING btree (correlation_id) WHERE (correlation_id IS NOT NULL)`
- `turn_audits_id_campaign_session_id_key` — `CREATE UNIQUE INDEX turn_audits_id_campaign_session_id_key ON public.turn_audits USING btree (id, campaign_session_id)`

</details>


## Campaign runtime heroes and rewards

### `encounter_reward_claims`

**Purpose.** Exactly-once record that a character claimed a server-derived reward from a committed encounter victory.

**Access pattern.** Reward application probes the campaign/encounter/character key, locks authoritative encounter/hero state, derives tier and XP server-side, updates the hero, writes a hero audit, and inserts this claim atomically. Unique keys prevent duplicate claims and preserve exact replay.

**Migration source(s).** `migrations/0006_encounter_reward_claims.sql`, `migrations/0008_major_encounter_reward.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/hero.rs:470` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1847` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE; FK → hero_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `encounter_id` | `text` | required; PK component; checked | Identifier for the associated encounter; used to scope, join, or correlate this row. |
| `character_id` | `text` | required; FK → hero_characters(id) ON DELETE CASCADE | Identifier for the associated character; used to scope, join, or correlate this row. |
| `encounter_revision` | `bigint` | required; checked | Authoritative encounter revision from which this result was derived. |
| `victory_event_sequence` | `bigint` | required; checked | Committed event sequence proving the encounter victory that authorized the reward. |
| `reward_tier` | `text` | required; checked | Server-derived trusted reward-policy tier for the committed encounter victory. |
| `experience_awarded` | `bigint` | required; checked | Exact server-derived XP applied to the runtime hero by this claim. |
| `hero_audit_id` | `text` | required; composite unique; FK → hero_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated hero audit; used to scope, join, or correlate this row. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `encounter_reward_claims_pkey` — `PRIMARY KEY (campaign_session_id, encounter_id)`
- `encounter_reward_claims_hero_audit_id_campaign_session_id_key` — `UNIQUE (hero_audit_id, campaign_session_id)`
- `encounter_reward_claims_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `encounter_reward_claims_character_id_fkey` — `FOREIGN KEY (character_id) REFERENCES hero_characters(id) ON DELETE CASCADE`
- `encounter_reward_claims_hero_audit_id_campaign_session_id_fkey` — `FOREIGN KEY (hero_audit_id, campaign_session_id) REFERENCES hero_audits(id, campaign_session_id) ON DELETE CASCADE`
- `encounter_reward_claims_encounter_id_check` — `CHECK (octet_length(encounter_id) >= 1 AND octet_length(encounter_id) <= 128 AND encounter_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `encounter_reward_claims_encounter_revision_check` — `CHECK (encounter_revision > 0)`
- `encounter_reward_claims_experience_awarded_check` — `CHECK (experience_awarded > 0)`
- `encounter_reward_claims_reward_tier_check` — `CHECK (reward_tier = ANY (ARRAY['minor'::text, 'major'::text]))`
- `encounter_reward_claims_victory_event_sequence_check` — `CHECK (victory_event_sequence > 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `encounter_reward_claims_character_idx` — `CREATE INDEX encounter_reward_claims_character_idx ON public.encounter_reward_claims USING btree (character_id, created_at DESC)`
- `encounter_reward_claims_hero_audit_id_campaign_session_id_key` — `CREATE UNIQUE INDEX encounter_reward_claims_hero_audit_id_campaign_session_id_key ON public.encounter_reward_claims USING btree (hero_audit_id, campaign_session_id)`

</details>

### `hero_audits`

**Purpose.** Append-only typed audit history for hero drafts, creation, advancement, and reward claims.

**Access pattern.** Hero transactions append at a subject revision; replay receipts and encounter claims reference the audit. History reads use campaign/subject order, and operations aggregate by `audit_kind`. Rows cascade with the campaign.

**Migration source(s).** `migrations/0004_hero_creation_and_advancement.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/hero.rs:737` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1795` (INSERT/SELECT), `crates/game-server/src/repository/operations.rs:395` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique; checked | Stable application-generated identifier for the `hero_audits` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `subject_kind` | `text` | required; composite unique; checked | Controlled subject kind discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `subject_id` | `text` | required; composite unique; checked | Identifier for the associated subject; used to scope, join, or correlate this row. |
| `audit_kind` | `text` | required; checked | Controlled hero-audit event discriminator. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `subject_revision` | `bigint` | required; composite unique; checked | Revision of the audited subject after the recorded operation. |
| `occurred_at_epoch_seconds` | `bigint` | required; checked | UTC Unix epoch seconds when occurred occurred or becomes effective. |
| `payload_json` | `jsonb` | required; checked | Typed minimized event payload appropriate to the hero audit kind. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `hero_audits_pkey` — `PRIMARY KEY (id)`
- `hero_audits_campaign_session_id_subject_kind_subject_id_sub_key` — `UNIQUE (campaign_session_id, subject_kind, subject_id, subject_revision)`
- `hero_audits_id_campaign_session_id_key` — `UNIQUE (id, campaign_session_id)`
- `hero_audits_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `hero_audits_audit_kind_check` — `CHECK (audit_kind = ANY (ARRAY['creation_transition'::text, 'reward_awarded'::text, 'level_up'::text]))`
- `hero_audits_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_audits_occurred_at_epoch_seconds_check` — `CHECK (occurred_at_epoch_seconds > 0)`
- `hero_audits_payload_json_check` — `CHECK (jsonb_typeof(payload_json) = 'object'::text AND octet_length(payload_json::text) >= 2 AND octet_length(payload_json::text) <= 131072)`
- `hero_audits_schema_version_check` — `CHECK (schema_version > 0)`
- `hero_audits_subject_id_check` — `CHECK (octet_length(subject_id) >= 1 AND octet_length(subject_id) <= 128 AND subject_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_audits_subject_kind_check` — `CHECK (subject_kind = ANY (ARRAY['draft'::text, 'character'::text]))`
- `hero_audits_subject_revision_check` — `CHECK (subject_revision > 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `hero_audits_campaign_session_id_subject_kind_subject_id_sub_key` — `CREATE UNIQUE INDEX hero_audits_campaign_session_id_subject_kind_subject_id_sub_key ON public.hero_audits USING btree (campaign_session_id, subject_kind, subject_id, subject_revision)`
- `hero_audits_id_campaign_session_id_key` — `CREATE UNIQUE INDEX hero_audits_id_campaign_session_id_key ON public.hero_audits USING btree (id, campaign_session_id)`
- `hero_audits_subject_idx` — `CREATE INDEX hero_audits_subject_idx ON public.hero_audits USING btree (campaign_session_id, subject_kind, subject_id, subject_revision, id)`

</details>

### `hero_characters`

**Purpose.** Campaign-specific authoritative runtime hero state, including level, XP, HP, derived sheet, and resources inside versioned JSON.

**Access pattern.** Loads are by ID or `(campaign_session_id, owner_key)`. Advancement/reward paths lock and update by expected revision while appending `hero_audits` and receipts. Character assignment can instantiate this row from `player_characters` in the same transaction as `campaign_character_instances`. Campaign deletion normally cascades it, but deletion of either the hero or its campaign is blocked while `custom_action_point_ledger.runtime_character_id` references the hero through a default `NO ACTION` FK.

**Migration source(s).** `migrations/0004_hero_creation_and_advancement.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/action_points.rs:340` (INSERT), `crates/game-server/src/repository/hero.rs:332` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/inspiration.rs:2710` (SELECT), `crates/game-server/src/repository/lifecycle.rs:1775` (INSERT/SELECT), `crates/game-server/src/repository/memberships.rs:828` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `hero_characters` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE; checked | Campaign that owns/scopes the row and is the principal partition key for access. |
| `owner_key` | `text` | required; composite unique; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `revision` | `bigint` | required; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `payload_json` | `jsonb` | required; checked | Versioned typed runtime hero state, including campaign-specific progression and mutable combat/resource values. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `hero_characters_pkey` — `PRIMARY KEY (id)`
- `hero_characters_one_owner_hero` — `UNIQUE (campaign_session_id, owner_key)`
- `hero_characters_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `hero_characters_campaign_session_id_check` — `CHECK (octet_length(campaign_session_id) >= 1 AND octet_length(campaign_session_id) <= 128)`
- `hero_characters_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_characters_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_characters_payload_json_check` — `CHECK (jsonb_typeof(payload_json) = 'object'::text AND octet_length(payload_json::text) >= 2 AND octet_length(payload_json::text) <= 65536)`
- `hero_characters_revision_check` — `CHECK (revision > 0)`
- `hero_characters_schema_version_check` — `CHECK (schema_version > 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `hero_characters_campaign_owner_idx` — `CREATE INDEX hero_characters_campaign_owner_idx ON public.hero_characters USING btree (campaign_session_id, owner_key, updated_at DESC)`
- `hero_characters_one_owner_hero` — `CREATE UNIQUE INDEX hero_characters_one_owner_hero ON public.hero_characters USING btree (campaign_session_id, owner_key)`

</details>

### `hero_command_receipts`

**Purpose.** Idempotency receipt shared by draft-, character-, and encounter-scoped hero commands.

**Access pattern.** The composite key `(scope_kind, scope_id, idempotency_key)` supports exact replay across several command kinds. Inserts occur in the same transaction as the hero mutation and audit; fingerprint drift fails closed. Receipts are immutable and tied to campaign/audit through FKs.

**Migration source(s).** `migrations/0004_hero_creation_and_advancement.sql`, `migrations/0006_encounter_reward_claims.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/hero.rs:442` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1821` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `scope_kind` | `text` | required; PK component; checked | Controlled scope kind discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `scope_id` | `text` | required; PK component; checked | Identifier for the associated scope; used to scope, join, or correlate this row. |
| `campaign_session_id` | `text` | required; FK → hero_audits(id, campaign_session_id) ON DELETE CASCADE; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `idempotency_key` | `text` | required; PK component; checked | Opaque client/operator retry key within the table’s documented scope. |
| `command_kind` | `text` | required; checked | Controlled command discriminator used during idempotent replay validation. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `expected_revision` | `bigint` | required; checked | Caller-expected revision checked before applying the command. |
| `result_revision` | `bigint` | required; checked | Revision returned after the idempotent command completed. |
| `audit_id` | `text` | required; FK → hero_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated audit; used to scope, join, or correlate this row. |
| `response_json` | `text` | required; checked | Bounded serialized response replayed for an exact duplicate command. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `hero_command_receipts_pkey` — `PRIMARY KEY (scope_kind, scope_id, idempotency_key)`
- `hero_command_receipts_audit_id_campaign_session_id_fkey` — `FOREIGN KEY (audit_id, campaign_session_id) REFERENCES hero_audits(id, campaign_session_id) ON DELETE CASCADE`
- `hero_command_receipts_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `hero_command_receipts_check` — `CHECK (result_revision = (expected_revision + 1))`
- `hero_command_receipts_command_kind_check` — `CHECK (command_kind = ANY (ARRAY['hero_creation_transition'::text, 'hero_reward'::text, 'hero_level_up'::text, 'encounter_reward_claim'::text]))`
- `hero_command_receipts_expected_revision_check` — `CHECK (expected_revision >= 0)`
- `hero_command_receipts_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_command_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `hero_command_receipts_response_json_check` — `CHECK (octet_length(response_json) >= 1 AND octet_length(response_json) <= 131072 AND jsonb_typeof(response_json::jsonb) IS NOT NULL)`
- `hero_command_receipts_scope_id_check` — `CHECK (octet_length(scope_id) >= 1 AND octet_length(scope_id) <= 128 AND scope_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_command_receipts_scope_kind_check` — `CHECK (scope_kind = ANY (ARRAY['draft'::text, 'character'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `hero_command_receipts_audit_idx` — `CREATE INDEX hero_command_receipts_audit_idx ON public.hero_command_receipts USING btree (audit_id, campaign_session_id)`

</details>

### `hero_creation_drafts`

**Purpose.** Expiring campaign-scoped draft for the older/typed runtime hero creation workflow.

**Access pattern.** Drafts are created and loaded by campaign/owner, selected by latest unexpired update, and saved with optimistic revision. Completion/deletion and retention cleanup remove rows; expiry and cleanup deadlines are stored as epoch seconds.

**Migration source(s).** `migrations/0004_hero_creation_and_advancement.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/application.rs:2680` (UPDATE), `crates/game-server/src/repository/hero.rs:262` (DELETE/INSERT/SELECT/UPDATE), `crates/game-server/src/repository/lifecycle.rs:1749` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `hero_creation_drafts` row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE; checked | Campaign that owns/scopes the row and is the principal partition key for access. |
| `owner_key` | `text` | required; checked | Legacy/local owner authorization partition retained for lifecycle and private-export compatibility. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `revision` | `bigint` | required; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `expires_at_epoch_seconds` | `bigint` | required; checked | UTC Unix epoch seconds when expires occurred or becomes effective. |
| `retention_delete_after_epoch_seconds` | `bigint` | required; checked | UTC Unix epoch seconds after which retention delete may occur. |
| `payload_json` | `jsonb` | required; checked | Versioned in-progress hero-creation selections and derived review state. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `hero_creation_drafts_pkey` — `PRIMARY KEY (id)`
- `hero_creation_drafts_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `hero_creation_drafts_campaign_session_id_check` — `CHECK (octet_length(campaign_session_id) >= 1 AND octet_length(campaign_session_id) <= 128)`
- `hero_creation_drafts_check` — `CHECK (retention_delete_after_epoch_seconds >= expires_at_epoch_seconds)`
- `hero_creation_drafts_expires_at_epoch_seconds_check` — `CHECK (expires_at_epoch_seconds > 0)`
- `hero_creation_drafts_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_creation_drafts_owner_key_check` — `CHECK (octet_length(owner_key) >= 1 AND octet_length(owner_key) <= 128 AND owner_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `hero_creation_drafts_payload_json_check` — `CHECK (jsonb_typeof(payload_json) = 'object'::text AND octet_length(payload_json::text) >= 2 AND octet_length(payload_json::text) <= 65536)`
- `hero_creation_drafts_revision_check` — `CHECK (revision > 0)`
- `hero_creation_drafts_schema_version_check` — `CHECK (schema_version > 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `hero_creation_drafts_owner_idx` — `CREATE INDEX hero_creation_drafts_owner_idx ON public.hero_creation_drafts USING btree (campaign_session_id, owner_key, updated_at DESC)`
- `hero_creation_drafts_retention_idx` — `CREATE INDEX hero_creation_drafts_retention_idx ON public.hero_creation_drafts USING btree (retention_delete_after_epoch_seconds)`

</details>


## Generated content queue and governance

### `generation_attempts`

**Purpose.** Per-try execution record beneath a generation job, including lease identity, provider/model, usage/cost, failure, artifact, and timing.

**Access pattern.** Claim inserts a running attempt; heartbeats and completion/failure update that exact lease-token row. Job reclaim closes expired attempts before opening a new one. History/operations join by `job_id`; attempts cascade when the parent job is deleted.

**Migration source(s).** `migrations/0005_generation_jobs.sql`, `migrations/0009_generated_text_presentations.sql`, `migrations/0012_generation_governance.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/governance.rs:557` (SELECT), `crates/game-server/src/repository/jobs.rs:763` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/operations.rs:425` (SELECT), `crates/game-server/src/repository/presentations.rs:292` (SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `generation_attempts` row. |
| `job_id` | `text` | required; composite unique; FK → generation_jobs(id) ON DELETE CASCADE | Identifier for the associated job; used to scope, join, or correlate this row. |
| `attempt_number` | `smallint` | required; composite unique; checked | One-based attempt ordinal within the parent generation job. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `lease_owner` | `text` | required; checked | Worker identity currently holding the job/attempt lease. |
| `lease_token` | `text` | required; unique; checked | Unpredictable token required for heartbeat and terminal updates by the lease holder. |
| `provider` | `text` | required; checked | Server-authoritative generation provider identity. |
| `model` | `text` | required; checked | Server-authoritative configured model identity. |
| `prompt_tokens` | `bigint` | nullable; checked | Provider-reported prompt/input token count for cost governance. |
| `completion_tokens` | `bigint` | nullable; checked | Provider-reported completion/output token count for cost governance. |
| `total_tokens` | `bigint` | nullable; checked | Provider-reported or derived total token count. |
| `cost_microusd` | `bigint` | nullable; checked | Generation cost in integer micro-US-dollars, avoiding floating-point currency. |
| `failure_class` | `text` | nullable; checked | Coarse retryability class for a failed generation attempt. |
| `failure_code` | `text` | nullable; checked | Stable machine-readable failure reason used for policy and metrics. |
| `provider_status` | `smallint` | nullable; checked | Optional provider HTTP/status code retained for minimized diagnostics. |
| `provider_request_id` | `text` | nullable; checked | Optional provider-issued request identifier for support correlation. |
| `artifact_id` | `text` | nullable; FK → generated_assets(id) ON DELETE RESTRICT; checked | Identifier for the associated artifact; used to scope, join, or correlate this row. |
| `started_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Timestamp when this execution attempt started. |
| `heartbeat_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Latest worker heartbeat proving the lease is still active. |
| `finished_at` | `timestamp with time zone` | nullable; checked | Timestamp when this execution attempt reached a terminal state. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `output_digest` | `text` | nullable; checked | Deterministic digest of output, used for integrity/equality checks without retaining the raw input. |
| `latency_milliseconds` | `bigint` | nullable; checked | Measured provider attempt latency in integer milliseconds. |

<details>
<summary>Exact table constraints</summary>

- `generation_attempts_pkey` — `PRIMARY KEY (id)`
- `generation_attempts_job_id_attempt_number_key` — `UNIQUE (job_id, attempt_number)`
- `generation_attempts_lease_token_key` — `UNIQUE (lease_token)`
- `generation_attempts_artifact_id_fkey` — `FOREIGN KEY (artifact_id) REFERENCES generated_assets(id) ON DELETE RESTRICT`
- `generation_attempts_job_id_fkey` — `FOREIGN KEY (job_id) REFERENCES generation_jobs(id) ON DELETE CASCADE`
- `generation_attempts_attempt_number_check` — `CHECK (attempt_number >= 1 AND attempt_number <= 5)`
- `generation_attempts_check` — `CHECK (total_tokens IS NULL OR prompt_tokens IS NULL OR completion_tokens IS NULL OR total_tokens >= (prompt_tokens + completion_tokens))`
- `generation_attempts_check1` — `CHECK (failure_class IS NULL AND failure_code IS NULL OR failure_class IS NOT NULL AND failure_code IS NOT NULL)`
- `generation_attempts_check2` — `CHECK (state = 'running'::text AND finished_at IS NULL AND failure_class IS NULL AND failure_code IS NULL AND artifact_id IS NULL OR state = 'succeeded'::text AND finished_at IS NOT NULL AND failure_class IS NULL AND failure_code IS NULL OR state = 'failed'::text AND finished_at IS NOT NULL AND failure_class IS NOT NULL AND failure_code IS NOT NULL AND artifact_id IS NULL OR state = 'cancelled'::text AND finished_at IS NOT NULL AND failure_class = 'permanent'::text AND failure_code = 'cancelled'::text AND artifact_id IS NULL)`
- `generation_attempts_completion_tokens_check` — `CHECK (completion_tokens IS NULL OR completion_tokens >= 0)`
- `generation_attempts_cost_microusd_check` — `CHECK (cost_microusd IS NULL OR cost_microusd >= 0)`
- `generation_attempts_failure_class_check` — `CHECK (failure_class IS NULL OR (failure_class = ANY (ARRAY['transient'::text, 'permanent'::text])))`
- `generation_attempts_failure_code_check` — `CHECK (failure_code IS NULL OR (failure_code = ANY (ARRAY['timeout'::text, 'provider_unavailable'::text, 'rate_limited'::text, 'provider_rejected'::text, 'malformed_response'::text, 'unsafe_output'::text, 'contradiction'::text, 'invalid_artifact'::text, 'budget_exceeded'::text, 'lease_expired'::text, 'cancelled'::text])))`
- `generation_attempts_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_attempts_latency_milliseconds_check` — `CHECK (latency_milliseconds IS NULL OR latency_milliseconds >= 0)`
- `generation_attempts_lease_owner_check` — `CHECK (octet_length(lease_owner) >= 1 AND octet_length(lease_owner) <= 128 AND lease_owner ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_attempts_lease_token_check` — `CHECK (octet_length(lease_token) >= 1 AND octet_length(lease_token) <= 128 AND lease_token ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_attempts_model_check` — `CHECK (octet_length(model) >= 1 AND octet_length(model) <= 256 AND model = btrim(model) AND model !~ '[[:cntrl:]]'::text)`
- `generation_attempts_output_digest_check` — `CHECK (output_digest IS NULL OR output_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_attempts_prompt_tokens_check` — `CHECK (prompt_tokens IS NULL OR prompt_tokens >= 0)`
- `generation_attempts_provider_check` — `CHECK (octet_length(provider) >= 1 AND octet_length(provider) <= 128 AND provider ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_attempts_provider_request_id_check` — `CHECK (provider_request_id IS NULL OR octet_length(provider_request_id) >= 1 AND octet_length(provider_request_id) <= 128 AND provider_request_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_attempts_provider_status_check` — `CHECK (provider_status IS NULL OR provider_status >= 100 AND provider_status <= 599)`
- `generation_attempts_state_check` — `CHECK (state = ANY (ARRAY['running'::text, 'succeeded'::text, 'failed'::text, 'cancelled'::text]))`
- `generation_attempts_total_tokens_check` — `CHECK (total_tokens IS NULL OR total_tokens >= 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generation_attempts_job_id_attempt_number_key` — `CREATE UNIQUE INDEX generation_attempts_job_id_attempt_number_key ON public.generation_attempts USING btree (job_id, attempt_number)`
- `generation_attempts_job_idx` — `CREATE INDEX generation_attempts_job_idx ON public.generation_attempts USING btree (job_id, attempt_number)`
- `generation_attempts_lease_token_key` — `CREATE UNIQUE INDEX generation_attempts_lease_token_key ON public.generation_attempts USING btree (lease_token)`
- `generation_attempts_provider_idx` — `CREATE INDEX generation_attempts_provider_idx ON public.generation_attempts USING btree (provider, model, created_at DESC)`

</details>

### `generation_governance_diagnostics`

**Purpose.** Minimized, short-lived record explaining generation requests rejected by governance limits.

**Access pattern.** Governance inserts on denial; operations aggregate by purpose/scope/dimension. Cleanup deletes rows after the default 14-day deadline using bounded `FOR UPDATE SKIP LOCKED` batches. No raw prompt or provider response is stored.

**Migration source(s).** `migrations/0012_generation_governance.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/governance.rs:570` (DELETE/INSERT/SELECT), `crates/game-server/src/repository/operations.rs:466` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `generation_governance_diagnostics` row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `purpose` | `text` | required; checked | Controlled generation purpose used for queue partitioning, policy, and budget aggregation. |
| `failure_code` | `text` | required; checked | Stable machine-readable failure reason used for policy and metrics. |
| `budget_scope` | `text` | required; checked | Controlled governance scope at which the request was denied (for example campaign or turn). |
| `budget_dimension` | `text` | required; checked | Controlled exhausted limit dimension such as requests, tokens, latency, cost, or concurrency. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `retention_delete_after` | `timestamp with time zone` | required; default `(CURRENT_TIMESTAMP + '14 days'::interval)` | Earliest timestamp at which bounded-retention cleanup may delete the row. |

<details>
<summary>Exact table constraints</summary>

- `generation_governance_diagnostics_pkey` — `PRIMARY KEY (id)`
- `generation_governance_diagnostics_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `generation_governance_diagnostics_budget_dimension_check` — `CHECK (budget_dimension = ANY (ARRAY['requests'::text, 'tokens'::text, 'latency'::text, 'cost'::text, 'concurrency'::text]))`
- `generation_governance_diagnostics_budget_scope_check` — `CHECK (budget_scope = ANY (ARRAY['turn'::text, 'campaign'::text, 'concurrency'::text]))`
- `generation_governance_diagnostics_failure_code_check` — `CHECK (failure_code = 'budget_exceeded'::text)`
- `generation_governance_diagnostics_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_governance_diagnostics_purpose_check` — `CHECK (purpose = ANY (ARRAY['intent_parsing'::text, 'gm_planning'::text, 'narration'::text, 'illustration'::text]))`
- `generation_governance_diagnostics_schema_version_check` — `CHECK (schema_version = 1)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generation_governance_diagnostics_metrics_idx` — `CREATE INDEX generation_governance_diagnostics_metrics_idx ON public.generation_governance_diagnostics USING btree (purpose, budget_scope, budget_dimension, created_at)`
- `generation_governance_diagnostics_retention_idx` — `CREATE INDEX generation_governance_diagnostics_retention_idx ON public.generation_governance_diagnostics USING btree (retention_delete_after, id)`

</details>

### `generation_governance_receipts`

**Purpose.** Durable budget/concurrency reservation and settlement record for one idempotent generation request.

**Access pattern.** Governance probes by campaign/purpose/key or `job_id`, sums active/spent reservations for limits, inserts a reserved row with the job, then updates it to settled/released after attempts. Selected operations lock the receipt. Rows cascade with the campaign/job and remain for governance evidence.

**Migration source(s).** `migrations/0012_generation_governance.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/generation_ledger.rs:804` (SELECT), `crates/game-server/src/repository/governance.rs:221` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/images.rs:118` (SELECT), `crates/game-server/src/repository/jobs.rs:1551` (SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `purpose` | `text` | required; PK component; checked | Controlled generation purpose used for queue partitioning, policy, and budget aggregation. |
| `idempotency_key` | `text` | required; PK component; checked | Opaque client/operator retry key within the table’s documented scope. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `job_id` | `text` | required; unique; checked | Identifier for the associated job; used to scope, join, or correlate this row. |
| `origin_turn_id` | `text` | nullable | Identifier for the associated origin turn; used to scope, join, or correlate this row. |
| `turn_scope_key` | `text` | required; checked | Deterministic scope key used to aggregate and enforce per-turn governance budgets, including non-turn requests. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `policy_fingerprint` | `text` | required; checked | Deterministic fingerprint of policy, used to detect replay, configuration, or policy drift. |
| `config_fingerprint` | `text` | required; checked | Deterministic fingerprint of config, used to detect replay, configuration, or policy drift. |
| `governance_fingerprint` | `text` | required; checked | Deterministic fingerprint of governance, used to detect replay, configuration, or policy drift. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `reserved_requests` | `smallint` | required; checked | Request-count capacity reserved before provider execution. |
| `reserved_tokens` | `bigint` | required; checked | Token capacity reserved before provider execution. |
| `reserved_latency_milliseconds` | `bigint` | required; checked | Latency budget reserved before provider execution. |
| `reserved_cost_microusd` | `bigint` | required; checked | Cost budget reserved before provider execution. |
| `spent_requests` | `smallint` | required; default `0`; checked | Settled request count consumed by the operation. |
| `spent_tokens` | `bigint` | required; default `0`; checked | Settled token count consumed by the operation. |
| `spent_latency_milliseconds` | `bigint` | required; default `0`; checked | Settled latency consumed by the operation. |
| `spent_cost_microusd` | `bigint` | required; default `0`; checked | Settled cost consumed by the operation. |
| `overage` | `boolean` | required; default `false` | Whether actual settled usage exceeded the reserved governance amount. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |
| `settled_at` | `timestamp with time zone` | nullable; checked | Timestamp when settled occurred or becomes effective. |

<details>
<summary>Exact table constraints</summary>

- `generation_governance_receipts_pkey` — `PRIMARY KEY (campaign_session_id, purpose, idempotency_key)`
- `generation_governance_receipts_job_id_key` — `UNIQUE (job_id)`
- `generation_governance_receipts_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `generation_governance_receip_reserved_latency_millisecond_check` — `CHECK (reserved_latency_milliseconds >= 0)`
- `generation_governance_receipts_check` — `CHECK (state = 'reserved'::text AND settled_at IS NULL OR (state = ANY (ARRAY['settled'::text, 'released'::text])) AND settled_at IS NOT NULL)`
- `generation_governance_receipts_config_fingerprint_check` — `CHECK (config_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_governance_receipts_governance_fingerprint_check` — `CHECK (governance_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_governance_receipts_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_governance_receipts_job_id_check` — `CHECK (octet_length(job_id) >= 1 AND octet_length(job_id) <= 128 AND job_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_governance_receipts_policy_fingerprint_check` — `CHECK (policy_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_governance_receipts_purpose_check` — `CHECK (purpose = ANY (ARRAY['intent_parsing'::text, 'gm_planning'::text, 'narration'::text, 'illustration'::text]))`
- `generation_governance_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_governance_receipts_reserved_cost_microusd_check` — `CHECK (reserved_cost_microusd >= 0)`
- `generation_governance_receipts_reserved_requests_check` — `CHECK (reserved_requests >= 0 AND reserved_requests <= 5)`
- `generation_governance_receipts_reserved_tokens_check` — `CHECK (reserved_tokens >= 0)`
- `generation_governance_receipts_schema_version_check` — `CHECK (schema_version = 1)`
- `generation_governance_receipts_spent_cost_microusd_check` — `CHECK (spent_cost_microusd >= 0)`
- `generation_governance_receipts_spent_latency_milliseconds_check` — `CHECK (spent_latency_milliseconds >= 0)`
- `generation_governance_receipts_spent_requests_check` — `CHECK (spent_requests >= 0 AND spent_requests <= 5)`
- `generation_governance_receipts_spent_tokens_check` — `CHECK (spent_tokens >= 0)`
- `generation_governance_receipts_state_check` — `CHECK (state = ANY (ARRAY['reserved'::text, 'settled'::text, 'released'::text]))`
- `generation_governance_receipts_turn_scope_key_check` — `CHECK (octet_length(turn_scope_key) >= 1 AND octet_length(turn_scope_key) <= 128 AND turn_scope_key ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generation_governance_campaign_idx` — `CREATE INDEX generation_governance_campaign_idx ON public.generation_governance_receipts USING btree (campaign_session_id, state, created_at)`
- `generation_governance_receipts_job_id_key` — `CREATE UNIQUE INDEX generation_governance_receipts_job_id_key ON public.generation_governance_receipts USING btree (job_id)`
- `generation_governance_turn_idx` — `CREATE INDEX generation_governance_turn_idx ON public.generation_governance_receipts USING btree (campaign_session_id, turn_scope_key, state)`

</details>

### `generation_jobs`

**Purpose.** Durable queue item for narration, illustration, and other generated-content work, including idempotency, lease, retry, provenance digest, and retention state.

**Access pattern.** Enqueue locks the campaign revision and inserts once per `(campaign, purpose, idempotency_key)`. Workers claim eligible rows with `FOR UPDATE SKIP LOCKED`, set leases, heartbeat, and transition queued/running to terminal states while creating attempts. Maintenance deletes terminal rows after retention deadlines in bounded batches.

**Migration source(s).** `migrations/0005_generation_jobs.sql`, `migrations/0009_generated_text_presentations.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/governance.rs:556` (DELETE/SELECT/UPDATE), `crates/game-server/src/repository/images.rs:142` (DELETE/SELECT/UPDATE), `crates/game-server/src/repository/jobs.rs:643` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/lifecycle.rs:4762` (SELECT), `crates/game-server/src/repository/operations.rs:364` (SELECT), `crates/game-server/src/repository/presentations.rs:257` (SELECT/UPDATE), `crates/game-server/src/scene_images.rs:1697` (UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; composite unique; checked | Stable application-generated identifier for the `generation_jobs` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `origin_turn_id` | `text` | nullable; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE; checked | Identifier for the associated origin turn; used to scope, join, or correlate this row. |
| `origin_campaign_revision` | `bigint` | required; checked | Captured or expected revision for origin campaign, used to reject stale operations. |
| `purpose` | `text` | required; composite unique; checked | Controlled generation purpose used for queue partitioning, policy, and budget aggregation. |
| `idempotency_key` | `text` | required; composite unique; checked | Opaque client/operator retry key within the table’s documented scope. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `input_digest` | `text` | required; checked | Digest of the canonical generation input; allows replay/drift checks without persisting raw prompt input. |
| `prompt_digest` | `text` | required; checked | Deterministic digest of prompt, used for integrity/equality checks without retaining the raw input. |
| `policy_digest` | `text` | required; checked | Deterministic digest of policy, used for integrity/equality checks without retaining the raw input. |
| `config_digest` | `text` | required; checked | Deterministic digest of config, used for integrity/equality checks without retaining the raw input. |
| `correlation_id` | `text` | nullable; checked | Request/trace correlation identifier used to connect audits and operational logs. |
| `attempt_count` | `smallint` | required; default `0`; checked | Number of attempts opened for the generation job. |
| `max_attempts` | `smallint` | required; checked | Maximum attempts policy allows before terminal failure. |
| `retry_at` | `timestamp with time zone` | nullable; checked | Earliest timestamp at which a queued/failed job may be claimed again. |
| `lease_owner` | `text` | nullable; checked | Worker identity currently holding the job/attempt lease. |
| `lease_token` | `text` | nullable; unique; checked | Unpredictable token required for heartbeat and terminal updates by the lease holder. |
| `lease_expires_at` | `timestamp with time zone` | nullable; checked | Worker lease deadline; an expired lease permits safe reclaim. |
| `last_failure_class` | `text` | nullable; checked | Failure class from the most recent job attempt. |
| `last_failure_code` | `text` | nullable; checked | Failure code from the most recent job attempt. |
| `artifact_id` | `text` | nullable; FK → generated_assets(id) ON DELETE RESTRICT; checked | Identifier for the associated artifact; used to scope, join, or correlate this row. |
| `success_retention_class` | `text` | required; checked | Controlled success retention class discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `retention_class` | `text` | required; default `'pending'`; checked | Controlled retention class discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `retention_delete_after` | `timestamp with time zone` | nullable; checked | Earliest timestamp at which bounded-retention cleanup may delete the row. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |
| `completed_at` | `timestamp with time zone` | nullable; checked | Timestamp when the workflow reached successful completion. |
| `output_digest` | `text` | nullable; checked | Deterministic digest of output, used for integrity/equality checks without retaining the raw input. |

<details>
<summary>Exact table constraints</summary>

- `generation_jobs_pkey` — `PRIMARY KEY (id)`
- `generation_jobs_campaign_session_id_purpose_idempotency_key_key` — `UNIQUE (campaign_session_id, purpose, idempotency_key)`
- `generation_jobs_id_campaign_session_id_key` — `UNIQUE (id, campaign_session_id)`
- `generation_jobs_lease_token_key` — `UNIQUE (lease_token)`
- `generation_jobs_artifact_id_fkey` — `FOREIGN KEY (artifact_id) REFERENCES generated_assets(id) ON DELETE RESTRICT`
- `generation_jobs_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `generation_jobs_origin_turn_id_campaign_session_id_fkey` — `FOREIGN KEY (origin_turn_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `generation_jobs_attempt_count_check` — `CHECK (attempt_count >= 0 AND attempt_count <= 5)`
- `generation_jobs_check` — `CHECK ((purpose <> ALL (ARRAY['narration'::text, 'illustration'::text])) OR origin_turn_id IS NOT NULL)`
- `generation_jobs_check1` — `CHECK (last_failure_class IS NULL AND last_failure_code IS NULL OR last_failure_class IS NOT NULL AND last_failure_code IS NOT NULL)`
- `generation_jobs_check2` — `CHECK (state = 'queued'::text AND retry_at IS NOT NULL AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL AND completed_at IS NULL AND artifact_id IS NULL AND retention_class = 'pending'::text AND retention_delete_after IS NULL AND attempt_count < max_attempts OR state = 'running'::text AND retry_at IS NULL AND lease_owner IS NOT NULL AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL AND completed_at IS NULL AND artifact_id IS NULL AND retention_class = 'pending'::text AND retention_delete_after IS NULL OR state = 'succeeded'::text AND retry_at IS NULL AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL AND completed_at IS NOT NULL AND last_failure_class IS NULL AND last_failure_code IS NULL AND (purpose <> 'illustration'::text OR artifact_id IS NOT NULL) AND (artifact_id IS NULL AND retention_class = 'unselected_presentation_30d'::text AND retention_delete_after IS NOT NULL OR artifact_id IS NOT NULL AND retention_class = success_retention_class AND (retention_class = 'unselected_presentation_30d'::text AND retention_delete_after IS NOT NULL OR retention_class = 'campaign_lifetime'::text AND retention_delete_after IS NULL)) OR (state = ANY (ARRAY['failed'::text, 'cancelled'::text])) AND retry_at IS NULL AND lease_owner IS NULL AND lease_token IS NULL AND lease_expires_at IS NULL AND completed_at IS NOT NULL AND artifact_id IS NULL AND last_failure_class IS NOT NULL AND last_failure_code IS NOT NULL AND retention_class = 'failed_metadata_7d'::text AND retention_delete_after IS NOT NULL)`
- `generation_jobs_config_digest_check` — `CHECK (config_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_jobs_correlation_id_check` — `CHECK (correlation_id IS NULL OR octet_length(correlation_id) >= 1 AND octet_length(correlation_id) <= 128 AND correlation_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_jobs_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_jobs_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_jobs_input_digest_check` — `CHECK (input_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_jobs_last_failure_class_check` — `CHECK (last_failure_class IS NULL OR (last_failure_class = ANY (ARRAY['transient'::text, 'permanent'::text])))`
- `generation_jobs_last_failure_code_check` — `CHECK (last_failure_code IS NULL OR (last_failure_code = ANY (ARRAY['timeout'::text, 'provider_unavailable'::text, 'rate_limited'::text, 'provider_rejected'::text, 'malformed_response'::text, 'unsafe_output'::text, 'contradiction'::text, 'invalid_artifact'::text, 'budget_exceeded'::text, 'lease_expired'::text, 'cancelled'::text])))`
- `generation_jobs_lease_owner_check` — `CHECK (lease_owner IS NULL OR octet_length(lease_owner) >= 1 AND octet_length(lease_owner) <= 128 AND lease_owner ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_jobs_lease_token_check` — `CHECK (lease_token IS NULL OR octet_length(lease_token) >= 1 AND octet_length(lease_token) <= 128 AND lease_token ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generation_jobs_max_attempts_check` — `CHECK (max_attempts >= 1 AND max_attempts <= 5)`
- `generation_jobs_origin_campaign_revision_check` — `CHECK (origin_campaign_revision > 0)`
- `generation_jobs_output_digest_check` — `CHECK (output_digest IS NULL OR output_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_jobs_policy_digest_check` — `CHECK (policy_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_jobs_prompt_digest_check` — `CHECK (prompt_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generation_jobs_purpose_check` — `CHECK (purpose = ANY (ARRAY['intent_parsing'::text, 'gm_planning'::text, 'narration'::text, 'illustration'::text]))`
- `generation_jobs_retention_class_check` — `CHECK (retention_class = ANY (ARRAY['pending'::text, 'failed_metadata_7d'::text, 'unselected_presentation_30d'::text, 'campaign_lifetime'::text]))`
- `generation_jobs_state_check` — `CHECK (state = ANY (ARRAY['queued'::text, 'running'::text, 'succeeded'::text, 'failed'::text, 'cancelled'::text]))`
- `generation_jobs_success_retention_class_check` — `CHECK (success_retention_class = ANY (ARRAY['unselected_presentation_30d'::text, 'campaign_lifetime'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generation_jobs_campaign_idx` — `CREATE INDEX generation_jobs_campaign_idx ON public.generation_jobs USING btree (campaign_session_id, created_at DESC, id)`
- `generation_jobs_campaign_session_id_purpose_idempotency_key_key` — `CREATE UNIQUE INDEX generation_jobs_campaign_session_id_purpose_idempotency_key_key ON public.generation_jobs USING btree (campaign_session_id, purpose, idempotency_key)`
- `generation_jobs_claim_idx` — `CREATE INDEX generation_jobs_claim_idx ON public.generation_jobs USING btree (retry_at, created_at, id) WHERE (state = 'queued'::text)`
- `generation_jobs_expired_lease_idx` — `CREATE INDEX generation_jobs_expired_lease_idx ON public.generation_jobs USING btree (lease_expires_at, created_at, id) WHERE (state = 'running'::text)`
- `generation_jobs_id_campaign_session_id_key` — `CREATE UNIQUE INDEX generation_jobs_id_campaign_session_id_key ON public.generation_jobs USING btree (id, campaign_session_id)`
- `generation_jobs_lease_token_key` — `CREATE UNIQUE INDEX generation_jobs_lease_token_key ON public.generation_jobs USING btree (lease_token)`
- `generation_jobs_retention_idx` — `CREATE INDEX generation_jobs_retention_idx ON public.generation_jobs USING btree (retention_delete_after) WHERE (retention_delete_after IS NOT NULL)`

</details>


## Generated presentations and scene images

### `generated_assets`

**Purpose.** Generic metadata catalog for durable generated artifacts, including scene-image roots and legacy/imported assets.

**Access pattern.** Generation completion inserts by artifact ID and campaign; campaign views list by campaign/turn. Scene-image publication upserts the generic row before its detailed artifact row and retention cleanup may delete it after dependent metadata is removed. Payload bytes live outside PostgreSQL at `location`/storage keys.

**Migration source(s).** `migrations/0001_server_storage.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository.rs:1008` (INSERT/SELECT), `crates/game-server/src/repository/images.rs:192` (DELETE/INSERT/SELECT), `crates/game-server/src/repository/jobs.rs:1079` (INSERT/SELECT), `crates/game-server/src/repository/legacy.rs:343` (INSERT/SELECT), `crates/game-server/src/repository/lifecycle.rs:1997` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK | Stable application-generated identifier for the `generated_assets` row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `turn_id` | `text` | nullable; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated turn; used to scope, join, or correlate this row. |
| `asset_kind` | `text` | required | Controlled asset kind discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `provider` | `text` | required | Server-authoritative generation provider identity. |
| `model` | `text` | required | Server-authoritative configured model identity. |
| `location` | `text` | required | External object/file location; PostgreSQL stores metadata, not artifact bytes. |
| `prompt_fingerprint` | `text` | nullable; checked | Deterministic fingerprint of prompt, used to detect replay, configuration, or policy drift. |
| `metadata_json` | `jsonb` | required; default `'{}'` | Bounded structured artifact metadata. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `generated_assets_pkey` — `PRIMARY KEY (id)`
- `generated_assets_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `generated_assets_turn_id_campaign_session_id_fkey` — `FOREIGN KEY (turn_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `generated_assets_prompt_fingerprint_check` — `CHECK (prompt_fingerprint IS NULL OR prompt_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generated_assets_campaign_session_idx` — `CREATE INDEX generated_assets_campaign_session_idx ON public.generated_assets USING btree (campaign_session_id, created_at)`
- `generated_assets_turn_idx` — `CREATE INDEX generated_assets_turn_idx ON public.generated_assets USING btree (turn_id)`

</details>

### `generated_text_presentation_receipts`

**Purpose.** Immutable lifetime replay record for text presentation publication, retained even if an unselected body expires.

**Access pattern.** Publication inserts by `(campaign_session_id, client_idempotency_key)` and also versions by origin turn. Retry paths load the receipt to return the original presentation identity/digests rather than regenerate content. It cascades with campaign/origin turn but is never updated.

**Migration source(s).** `migrations/0011_generated_text_idempotency_receipts.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:4576` (SELECT), `crates/game-server/src/repository/lifecycle.rs:1083` (INSERT/SELECT), `crates/game-server/src/repository/presentations.rs:373` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; composite unique; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `origin_turn_id` | `text` | required; PK component; composite unique; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated origin turn; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `client_idempotency_key` | `text` | required; PK component; checked | Opaque client retry key used to return the original result without repeating work. |
| `presentation_id` | `text` | required; unique; checked | Identifier for the associated presentation; used to scope, join, or correlate this row. |
| `generation_job_id` | `text` | required; unique; checked | Identifier for the associated generation job; used to scope, join, or correlate this row. |
| `generation_attempt_id` | `text` | required; unique; checked | Identifier for the associated generation attempt; used to scope, join, or correlate this row. |
| `version` | `smallint` | required; composite unique; checked | Monotonic presentation version within the origin turn. |
| `source` | `text` | required; checked | Controlled presentation source (for example provider output versus deterministic fallback). |
| `config_digest` | `text` | required; checked | Deterministic digest of config, used for integrity/equality checks without retaining the raw input. |
| `prompt_digest` | `text` | required; checked | Deterministic digest of prompt, used for integrity/equality checks without retaining the raw input. |
| `policy_digest` | `text` | required; checked | Deterministic digest of policy, used for integrity/equality checks without retaining the raw input. |
| `output_digest` | `text` | required; checked | Deterministic digest of output, used for integrity/equality checks without retaining the raw input. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `generated_text_presentation_receipts_pkey` — `PRIMARY KEY (campaign_session_id, origin_turn_id, client_idempotency_key)`
- `generated_text_presentation_r_campaign_session_id_origin_tu_key` — `UNIQUE (campaign_session_id, origin_turn_id, version)`
- `generated_text_presentation_receipts_generation_attempt_id_key` — `UNIQUE (generation_attempt_id)`
- `generated_text_presentation_receipts_generation_job_id_key` — `UNIQUE (generation_job_id)`
- `generated_text_presentation_receipts_presentation_id_key` — `UNIQUE (presentation_id)`
- `generated_text_presentation_r_origin_turn_id_campaign_sess_fkey` — `FOREIGN KEY (origin_turn_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `generated_text_presentation_receipts_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `generated_text_presentation_receip_client_idempotency_key_check` — `CHECK (octet_length(client_idempotency_key) >= 1 AND octet_length(client_idempotency_key) <= 128 AND client_idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentation_receipt_generation_attempt_id_check` — `CHECK (octet_length(generation_attempt_id) >= 1 AND octet_length(generation_attempt_id) <= 128 AND generation_attempt_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentation_receipts_config_digest_check` — `CHECK (config_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentation_receipts_generation_job_id_check` — `CHECK (octet_length(generation_job_id) >= 1 AND octet_length(generation_job_id) <= 128 AND generation_job_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentation_receipts_output_digest_check` — `CHECK (output_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentation_receipts_policy_digest_check` — `CHECK (policy_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentation_receipts_presentation_id_check` — `CHECK (octet_length(presentation_id) >= 1 AND octet_length(presentation_id) <= 128 AND presentation_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentation_receipts_prompt_digest_check` — `CHECK (prompt_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentation_receipts_schema_version_check` — `CHECK (schema_version = 1)`
- `generated_text_presentation_receipts_source_check` — `CHECK (source = ANY (ARRAY['provider'::text, 'authored_fallback'::text, 'engine_authored'::text]))`
- `generated_text_presentation_receipts_version_check` — `CHECK (version >= 1 AND version <= 3)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generated_text_presentation_r_campaign_session_id_origin_tu_key` — `CREATE UNIQUE INDEX generated_text_presentation_r_campaign_session_id_origin_tu_key ON public.generated_text_presentation_receipts USING btree (campaign_session_id, origin_turn_id, version)`
- `generated_text_presentation_receipts_generation_attempt_id_key` — `CREATE UNIQUE INDEX generated_text_presentation_receipts_generation_attempt_id_key ON public.generated_text_presentation_receipts USING btree (generation_attempt_id)`
- `generated_text_presentation_receipts_generation_job_id_key` — `CREATE UNIQUE INDEX generated_text_presentation_receipts_generation_job_id_key ON public.generated_text_presentation_receipts USING btree (generation_job_id)`
- `generated_text_presentation_receipts_presentation_id_key` — `CREATE UNIQUE INDEX generated_text_presentation_receipts_presentation_id_key ON public.generated_text_presentation_receipts USING btree (presentation_id)`
- `generated_text_presentation_receipts_turn_idx` — `CREATE INDEX generated_text_presentation_receipts_turn_idx ON public.generated_text_presentation_receipts USING btree (campaign_session_id, origin_turn_id, version)`

</details>

### `generated_text_presentations`

**Purpose.** Retained, owner-visible version of generated text for one committed origin turn.

**Access pattern.** Publication validates the running job/attempt and origin turn under locks, supersedes the previous selected version, inserts a new version, and writes an immutable presentation receipt atomically. Reads list versions by campaign/turn or exact job/attempt/client key. Unselected versions are deleted after retention via `SKIP LOCKED`; one selected version is retained.

**Migration source(s).** `migrations/0009_generated_text_presentations.sql`, `migrations/0015_private_inspiration_presentations.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:656` (DELETE/SELECT/UPDATE), `crates/game-server/src/repository/lifecycle.rs:1067` (INSERT/SELECT), `crates/game-server/src/repository/presentations.rs:405` (DELETE/INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `generated_text_presentations` row. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `origin_turn_id` | `text` | required; composite unique; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated origin turn; used to scope, join, or correlate this row. |
| `generation_job_id` | `text` | required; unique; checked | Identifier for the associated generation job; used to scope, join, or correlate this row. |
| `generation_attempt_id` | `text` | required; unique; checked | Identifier for the associated generation attempt; used to scope, join, or correlate this row. |
| `client_idempotency_key` | `text` | required; composite unique; checked | Opaque client retry key used to return the original result without repeating work. |
| `version` | `smallint` | required; composite unique; checked | Monotonic presentation version within the origin turn. |
| `source` | `text` | required; checked | Controlled presentation source (for example provider output versus deterministic fallback). |
| `body` | `text` | required; checked | Owner-visible generated text body for this retained version. |
| `config_digest` | `text` | required; checked | Deterministic digest of config, used for integrity/equality checks without retaining the raw input. |
| `prompt_digest` | `text` | required; checked | Deterministic digest of prompt, used for integrity/equality checks without retaining the raw input. |
| `policy_digest` | `text` | required; checked | Deterministic digest of policy, used for integrity/equality checks without retaining the raw input. |
| `output_digest` | `text` | required; checked | Deterministic digest of output, used for integrity/equality checks without retaining the raw input. |
| `selected` | `boolean` | required; checked | Whether this is the currently owner-selected presentation version. |
| `retention_delete_after` | `timestamp with time zone` | nullable; checked | Earliest timestamp at which bounded-retention cleanup may delete the row. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |
| `private_inspiration_work_id` | `text` | nullable; unique; FK → private_inspiration_derived_work(work_id) ON DELETE SET NULL | Identifier for the associated private inspiration work; used to scope, join, or correlate this row. |
| `privacy_state` | `text` | required; default `'visible'`; checked | Visibility/privacy lifecycle for a presentation linked to private inspiration. |

<details>
<summary>Exact table constraints</summary>

- `generated_text_presentations_pkey` — `PRIMARY KEY (id)`
- `generated_text_presentations_campaign_session_id_origin_tu_key1` — `UNIQUE (campaign_session_id, origin_turn_id, version)`
- `generated_text_presentations_campaign_session_id_origin_tur_key` — `UNIQUE (campaign_session_id, origin_turn_id, client_idempotency_key)`
- `generated_text_presentations_generation_attempt_id_key` — `UNIQUE (generation_attempt_id)`
- `generated_text_presentations_generation_job_id_key` — `UNIQUE (generation_job_id)`
- `generated_text_presentations_private_inspiration_work_id_key` — `UNIQUE (private_inspiration_work_id)`
- `generated_text_presentations_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `generated_text_presentations_origin_turn_id_campaign_sessi_fkey` — `FOREIGN KEY (origin_turn_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `generated_text_presentations_private_inspiration_work_id_fkey` — `FOREIGN KEY (private_inspiration_work_id) REFERENCES private_inspiration_derived_work(work_id) ON DELETE SET NULL`
- `generated_text_presentations_body_check` — `CHECK (body = btrim(body) AND char_length(body) >= 1 AND char_length(body) <= 12000 AND octet_length(body) <= 49152)`
- `generated_text_presentations_check` — `CHECK (selected AND retention_delete_after IS NULL OR NOT selected AND retention_delete_after IS NOT NULL)`
- `generated_text_presentations_client_idempotency_key_check` — `CHECK (octet_length(client_idempotency_key) >= 1 AND octet_length(client_idempotency_key) <= 128 AND client_idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentations_config_digest_check` — `CHECK (config_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentations_generation_attempt_id_check` — `CHECK (octet_length(generation_attempt_id) >= 1 AND octet_length(generation_attempt_id) <= 128 AND generation_attempt_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentations_generation_job_id_check` — `CHECK (octet_length(generation_job_id) >= 1 AND octet_length(generation_job_id) <= 128 AND generation_job_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentations_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `generated_text_presentations_output_digest_check` — `CHECK (output_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentations_policy_digest_check` — `CHECK (policy_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentations_privacy_redaction_check` — `CHECK (privacy_state = 'visible'::text OR body = 'Private inspiration removed at a participant request. The committed game mechanics are unchanged.'::text)`
- `generated_text_presentations_privacy_state_check` — `CHECK (privacy_state = ANY (ARRAY['visible'::text, 'redacted'::text]))`
- `generated_text_presentations_prompt_digest_check` — `CHECK (prompt_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `generated_text_presentations_source_check` — `CHECK (source = ANY (ARRAY['provider'::text, 'authored_fallback'::text, 'engine_authored'::text]))`
- `generated_text_presentations_version_check` — `CHECK (version >= 1 AND version <= 3)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `generated_text_presentations_campaign_session_id_origin_tu_key1` — `CREATE UNIQUE INDEX generated_text_presentations_campaign_session_id_origin_tu_key1 ON public.generated_text_presentations USING btree (campaign_session_id, origin_turn_id, version)`
- `generated_text_presentations_campaign_session_id_origin_tur_key` — `CREATE UNIQUE INDEX generated_text_presentations_campaign_session_id_origin_tur_key ON public.generated_text_presentations USING btree (campaign_session_id, origin_turn_id, client_idempotency_key)`
- `generated_text_presentations_generation_attempt_id_key` — `CREATE UNIQUE INDEX generated_text_presentations_generation_attempt_id_key ON public.generated_text_presentations USING btree (generation_attempt_id)`
- `generated_text_presentations_generation_job_id_key` — `CREATE UNIQUE INDEX generated_text_presentations_generation_job_id_key ON public.generated_text_presentations USING btree (generation_job_id)`
- `generated_text_presentations_private_inspiration_work_id_key` — `CREATE UNIQUE INDEX generated_text_presentations_private_inspiration_work_id_key ON public.generated_text_presentations USING btree (private_inspiration_work_id)`
- `generated_text_presentations_retention_idx` — `CREATE INDEX generated_text_presentations_retention_idx ON public.generated_text_presentations USING btree (retention_delete_after) WHERE (retention_delete_after IS NOT NULL)`
- `generated_text_presentations_selected_idx` — `CREATE UNIQUE INDEX generated_text_presentations_selected_idx ON public.generated_text_presentations USING btree (campaign_session_id, origin_turn_id) WHERE selected`
- `generated_text_presentations_turn_idx` — `CREATE INDEX generated_text_presentations_turn_idx ON public.generated_text_presentations USING btree (campaign_session_id, origin_turn_id, version)`

</details>

### `scene_image_artifacts`

**Purpose.** Detailed durable metadata for validated scene-image variants and their selection/provenance state.

**Access pattern.** Publication upserts by artifact ID while the generation job is still authoritative. Reads join `generation_jobs` for visibility/retention checks. Selection transaction marks prior turn images superseded, selects one row, and updates job retention classes. Operations/export scan selected campaign artifacts. Actual image bytes are external.

**Migration source(s).** `migrations/0021_scene_image_artifacts.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/images.rs:219` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/operations.rs:260` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `artifact_id` | `text` | required; PK; composite unique; FK → generated_assets(id) ON DELETE CASCADE | Identifier for the associated artifact; used to scope, join, or correlate this row. |
| `job_id` | `text` | required; unique; FK → generation_jobs(id, campaign_session_id) ON DELETE CASCADE; FK → generation_jobs(id) ON DELETE CASCADE | Identifier for the associated job; used to scope, join, or correlate this row. |
| `campaign_session_id` | `text` | required; composite unique; FK → generation_jobs(id, campaign_session_id) ON DELETE CASCADE; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `source_turn_id` | `text` | required; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Identifier for the associated source turn; used to scope, join, or correlate this row. |
| `schema_version` | `smallint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `brief_fingerprint` | `text` | required; checked | Deterministic fingerprint of brief, used to detect replay, configuration, or policy drift. |
| `prompt_policy_fingerprint` | `text` | required; checked | Deterministic fingerprint of prompt policy, used to detect replay, configuration, or policy drift. |
| `config_fingerprint` | `text` | required; checked | Deterministic fingerprint of config, used to detect replay, configuration, or policy drift. |
| `original_storage_key` | `text` | required; checked | Protected external storage key for the original validated image variant. |
| `web_storage_key` | `text` | required; checked | External storage key for the web-sized image variant. |
| `thumbnail_storage_key` | `text` | required; checked | External storage key for the thumbnail image variant. |
| `original_digest` | `text` | required; checked | Deterministic digest of original, used for integrity/equality checks without retaining the raw input. |
| `web_digest` | `text` | required; checked | Deterministic digest of web, used for integrity/equality checks without retaining the raw input. |
| `thumbnail_digest` | `text` | required; checked | Deterministic digest of thumbnail, used for integrity/equality checks without retaining the raw input. |
| `media_type` | `text` | required; checked | Validated MIME/media type of the stored image variants. |
| `original_width` | `integer` | required; checked | Pixel width of the original image variant. |
| `original_height` | `integer` | required; checked | Pixel height of the original image variant. |
| `web_width` | `integer` | required; checked | Pixel width of the web image variant. |
| `web_height` | `integer` | required; checked | Pixel height of the web image variant. |
| `thumbnail_width` | `integer` | required; checked | Pixel width of the thumbnail image variant. |
| `thumbnail_height` | `integer` | required; checked | Pixel height of the thumbnail image variant. |
| `alt_text` | `text` | required; checked | Player-visible accessibility description for the image. |
| `moderation_result` | `text` | required; checked | Controlled safety/moderation outcome required before publication. |
| `selection_state` | `text` | required; checked | Whether the image is selected, superseded, or otherwise retained but not visible. |
| `estimated_cost_microusd` | `bigint` | required; checked | Pre-publication estimated image cost in integer micro-US-dollars. |
| `actual_cost_microusd` | `bigint` | nullable; checked | Settled image cost in integer micro-US-dollars, when known. |
| `license_id` | `text` | required; checked | Controlled license/provenance policy identifier for the artifact. |
| `provenance_summary` | `text` | required; checked | Safe textual provenance summary exposed with the artifact; excludes credentials and raw provider response. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `published_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Timestamp when the validated artifact became publishable. |

<details>
<summary>Exact table constraints</summary>

- `scene_image_artifacts_pkey` — `PRIMARY KEY (artifact_id)`
- `scene_image_artifacts_artifact_id_campaign_session_id_key` — `UNIQUE (artifact_id, campaign_session_id)`
- `scene_image_artifacts_job_id_key` — `UNIQUE (job_id)`
- `scene_image_artifacts_artifact_id_fkey` — `FOREIGN KEY (artifact_id) REFERENCES generated_assets(id) ON DELETE CASCADE`
- `scene_image_artifacts_job_id_campaign_session_id_fkey` — `FOREIGN KEY (job_id, campaign_session_id) REFERENCES generation_jobs(id, campaign_session_id) ON DELETE CASCADE`
- `scene_image_artifacts_job_id_fkey` — `FOREIGN KEY (job_id) REFERENCES generation_jobs(id) ON DELETE CASCADE`
- `scene_image_artifacts_source_turn_id_campaign_session_id_fkey` — `FOREIGN KEY (source_turn_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `scene_image_artifacts_actual_cost_microusd_check` — `CHECK (actual_cost_microusd IS NULL OR actual_cost_microusd >= 0)`
- `scene_image_artifacts_alt_text_check` — `CHECK (alt_text = btrim(alt_text) AND char_length(alt_text) >= 1 AND char_length(alt_text) <= 500 AND alt_text !~ '[[:cntrl:]]'::text)`
- `scene_image_artifacts_brief_fingerprint_check` — `CHECK (brief_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_artifacts_config_fingerprint_check` — `CHECK (config_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_artifacts_estimated_cost_microusd_check` — `CHECK (estimated_cost_microusd >= 0)`
- `scene_image_artifacts_license_id_check` — `CHECK (license_id = ANY (ARRAY['provider-output-operator-terms'::text, 'deterministic-fake-fixture'::text]))`
- `scene_image_artifacts_media_type_check` — `CHECK (media_type = 'image/png'::text)`
- `scene_image_artifacts_moderation_result_check` — `CHECK (moderation_result = 'provider_and_application_safe'::text)`
- `scene_image_artifacts_original_digest_check` — `CHECK (original_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_artifacts_original_height_check` — `CHECK (original_height >= 1 AND original_height <= 4096)`
- `scene_image_artifacts_original_storage_key_check` — `CHECK (octet_length(original_storage_key) >= 1 AND octet_length(original_storage_key) <= 512 AND original_storage_key ~ '^[A-Za-z0-9._/-]+$'::text AND original_storage_key !~ '(^\|/)\.\.(/\|$)'::text)`
- `scene_image_artifacts_original_width_check` — `CHECK (original_width >= 1 AND original_width <= 4096)`
- `scene_image_artifacts_prompt_policy_fingerprint_check` — `CHECK (prompt_policy_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_artifacts_provenance_summary_check` — `CHECK (provenance_summary = ANY (ARRAY['generated-from-committed-public-fictional-facts'::text, 'deterministic-network-free-test-fixture'::text]))`
- `scene_image_artifacts_schema_version_check` — `CHECK (schema_version = 1)`
- `scene_image_artifacts_selection_state_check` — `CHECK (selection_state = ANY (ARRAY['selected'::text, 'superseded'::text]))`
- `scene_image_artifacts_thumbnail_digest_check` — `CHECK (thumbnail_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_artifacts_thumbnail_height_check` — `CHECK (thumbnail_height >= 1 AND thumbnail_height <= 512)`
- `scene_image_artifacts_thumbnail_storage_key_check` — `CHECK (octet_length(thumbnail_storage_key) >= 1 AND octet_length(thumbnail_storage_key) <= 512 AND thumbnail_storage_key ~ '^[A-Za-z0-9._/-]+$'::text AND thumbnail_storage_key !~ '(^\|/)\.\.(/\|$)'::text)`
- `scene_image_artifacts_thumbnail_width_check` — `CHECK (thumbnail_width >= 1 AND thumbnail_width <= 512)`
- `scene_image_artifacts_web_digest_check` — `CHECK (web_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_artifacts_web_height_check` — `CHECK (web_height >= 1 AND web_height <= 1600)`
- `scene_image_artifacts_web_storage_key_check` — `CHECK (octet_length(web_storage_key) >= 1 AND octet_length(web_storage_key) <= 512 AND web_storage_key ~ '^[A-Za-z0-9._/-]+$'::text AND web_storage_key !~ '(^\|/)\.\.(/\|$)'::text)`
- `scene_image_artifacts_web_width_check` — `CHECK (web_width >= 1 AND web_width <= 1600)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `scene_image_artifacts_artifact_id_campaign_session_id_key` — `CREATE UNIQUE INDEX scene_image_artifacts_artifact_id_campaign_session_id_key ON public.scene_image_artifacts USING btree (artifact_id, campaign_session_id)`
- `scene_image_artifacts_job_id_key` — `CREATE UNIQUE INDEX scene_image_artifacts_job_id_key ON public.scene_image_artifacts USING btree (job_id)`
- `scene_image_campaign_created_idx` — `CREATE INDEX scene_image_campaign_created_idx ON public.scene_image_artifacts USING btree (campaign_session_id, created_at DESC, artifact_id)`
- `scene_image_one_selected_turn_idx` — `CREATE UNIQUE INDEX scene_image_one_selected_turn_idx ON public.scene_image_artifacts USING btree (campaign_session_id, source_turn_id) WHERE (selection_state = 'selected'::text)`

</details>

### `scene_image_quarantines`

**Purpose.** Short-lived metadata for rejected or unsafe scene-image bytes, with optional protected storage location.

**Access pattern.** Image validation inserts a minimized row for failures. Cleanup selects expired rows ordered by deadline, removes external bytes first, then deletes the matching row. Default retention is 14 days; no quarantine row is presented to players.

**Migration source(s).** `migrations/0021_scene_image_artifacts.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/images.rs:492` (DELETE/INSERT/SELECT), `crates/game-server/src/scene_images.rs:1724` (SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK; checked | Stable application-generated identifier for the `scene_image_quarantines` row. |
| `job_id` | `text` | required; composite unique; checked | Identifier for the associated job; used to scope, join, or correlate this row. |
| `attempt_id` | `text` | required; composite unique; checked | Identifier for the associated attempt; used to scope, join, or correlate this row. |
| `campaign_session_id` | `text` | required; checked | Campaign that owns/scopes the row and is the principal partition key for access. |
| `byte_digest` | `text` | nullable; checked | Deterministic digest of byte, used for integrity/equality checks without retaining the raw input. |
| `byte_length` | `bigint` | nullable; checked | Byte/item length for byte, used for validation and cleanup evidence. |
| `storage_key` | `text` | nullable; checked | Optional protected external location of quarantined bytes pending deletion. |
| `reason_code` | `text` | required; checked | Controlled reason explaining why material was quarantined. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `delete_after` | `timestamp with time zone` | required; default `(CURRENT_TIMESTAMP + '14 days'::interval)` | Earliest timestamp at which cleanup may delete this row and any external material. |

<details>
<summary>Exact table constraints</summary>

- `scene_image_quarantines_pkey` — `PRIMARY KEY (id)`
- `scene_image_quarantines_job_id_attempt_id_key` — `UNIQUE (job_id, attempt_id)`
- `scene_image_quarantines_attempt_id_check` — `CHECK (octet_length(attempt_id) >= 1 AND octet_length(attempt_id) <= 128 AND attempt_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `scene_image_quarantines_byte_digest_check` — `CHECK (byte_digest IS NULL OR byte_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `scene_image_quarantines_byte_length_check` — `CHECK (byte_length IS NULL OR byte_length >= 0 AND byte_length <= 33554432)`
- `scene_image_quarantines_campaign_session_id_check` — `CHECK (octet_length(campaign_session_id) >= 1 AND octet_length(campaign_session_id) <= 128 AND campaign_session_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `scene_image_quarantines_id_check` — `CHECK (octet_length(id) >= 1 AND octet_length(id) <= 128 AND id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `scene_image_quarantines_job_id_check` — `CHECK (octet_length(job_id) >= 1 AND octet_length(job_id) <= 128 AND job_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `scene_image_quarantines_reason_code_check` — `CHECK (reason_code = ANY (ARRAY['provider_url_rejected'::text, 'base64_invalid'::text, 'byte_limit'::text, 'format_invalid'::text, 'dimensions_invalid'::text, 'pixel_limit'::text, 'decode_failed'::text, 'safety_rejected'::text]))`
- `scene_image_quarantines_storage_key_check` — `CHECK (storage_key IS NULL OR octet_length(storage_key) >= 1 AND octet_length(storage_key) <= 512 AND storage_key ~ '^[A-Za-z0-9._/-]+$'::text AND storage_key !~ '(^\|/)\.\.(/\|$)'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `scene_image_quarantine_expiry_idx` — `CREATE INDEX scene_image_quarantine_expiry_idx ON public.scene_image_quarantines USING btree (delete_after, id)`
- `scene_image_quarantines_job_id_attempt_id_key` — `CREATE UNIQUE INDEX scene_image_quarantines_job_id_attempt_id_key ON public.scene_image_quarantines USING btree (job_id, attempt_id)`

</details>

### `typed_intent_command_receipts`

**Purpose.** Two-phase idempotency record for interpreting and committing a typed free-form player intent.

**Access pattern.** Validation inserts a `pending` receipt keyed by campaign/client key with resolved intent and evidence. The mechanics transaction later updates that exact row to `committed` with origin turn/event/revision. Replay reads may lock the row; mismatched digests/revisions fail closed.

**Migration source(s).** `migrations/0011_generated_text_idempotency_receipts.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/lifecycle.rs:1115` (INSERT/SELECT), `crates/game-server/src/repository/presentations.rs:703` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `client_idempotency_key` | `text` | required; PK component; checked | Opaque client retry key used to return the original result without repeating work. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `player_intent_digest` | `text` | required; checked | Deterministic digest of player intent, used for integrity/equality checks without retaining the raw input. |
| `expected_campaign_revision` | `bigint` | required; checked | Campaign aggregate revision required before committing this operation. |
| `expected_encounter_revision` | `bigint` | required; checked | Encounter revision required before resolving/committing the intent. |
| `resolved_intent_json` | `jsonb` | required; checked | Server-validated structured intent that mechanics code may execute; retained for exact replay. |
| `interpretation_label` | `text` | required; checked | Bounded player-visible/server-auditable label for the validated intent interpretation. |
| `interpretation_evidence_json` | `jsonb` | required; checked | Bounded evidence explaining how the free-form intent was mapped to the allowed action. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `origin_turn_id` | `text` | nullable; composite unique; FK → turn_audits(id, campaign_session_id) ON DELETE CASCADE; checked | Identifier for the associated origin turn; used to scope, join, or correlate this row. |
| `event_sequence` | `bigint` | nullable; checked | Committed event sequence linking the receipt to the exact turn result. |
| `result_campaign_revision` | `bigint` | nullable; checked | Campaign aggregate revision produced by the committed operation. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `typed_intent_command_receipts_pkey` — `PRIMARY KEY (campaign_session_id, client_idempotency_key)`
- `typed_intent_command_receipts_origin_turn_id_campaign_sessi_key` — `UNIQUE (origin_turn_id, campaign_session_id)`
- `typed_intent_command_receipts_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `typed_intent_command_receipts_origin_turn_id_campaign_sess_fkey` — `FOREIGN KEY (origin_turn_id, campaign_session_id) REFERENCES turn_audits(id, campaign_session_id) ON DELETE CASCADE`
- `typed_intent_command_receipt_interpretation_evidence_json_check` — `CHECK (jsonb_typeof(interpretation_evidence_json) = 'object'::text AND octet_length(interpretation_evidence_json::text) >= 2 AND octet_length(interpretation_evidence_json::text) <= 32768)`
- `typed_intent_command_receipts_check` — `CHECK (state = 'pending'::text AND origin_turn_id IS NULL AND event_sequence IS NULL AND result_campaign_revision IS NULL OR state = 'committed'::text AND origin_turn_id IS NOT NULL AND event_sequence IS NOT NULL AND result_campaign_revision = (expected_campaign_revision + 1))`
- `typed_intent_command_receipts_client_idempotency_key_check` — `CHECK (octet_length(client_idempotency_key) >= 1 AND octet_length(client_idempotency_key) <= 128 AND client_idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `typed_intent_command_receipts_event_sequence_check` — `CHECK (event_sequence > 0)`
- `typed_intent_command_receipts_expected_campaign_revision_check` — `CHECK (expected_campaign_revision > 0)`
- `typed_intent_command_receipts_expected_encounter_revision_check` — `CHECK (expected_encounter_revision > 0)`
- `typed_intent_command_receipts_interpretation_label_check` — `CHECK (interpretation_label = btrim(interpretation_label) AND char_length(interpretation_label) >= 1 AND char_length(interpretation_label) <= 512 AND octet_length(interpretation_label) <= 2048)`
- `typed_intent_command_receipts_player_intent_digest_check` — `CHECK (player_intent_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `typed_intent_command_receipts_resolved_intent_json_check` — `CHECK (jsonb_typeof(resolved_intent_json) = 'object'::text AND octet_length(resolved_intent_json::text) >= 2 AND octet_length(resolved_intent_json::text) <= 8192)`
- `typed_intent_command_receipts_result_campaign_revision_check` — `CHECK (result_campaign_revision > 0)`
- `typed_intent_command_receipts_schema_version_check` — `CHECK (schema_version = 1)`
- `typed_intent_command_receipts_state_check` — `CHECK (state = ANY (ARRAY['pending'::text, 'committed'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `typed_intent_command_receipts_origin_turn_id_campaign_sessi_key` — `CREATE UNIQUE INDEX typed_intent_command_receipts_origin_turn_id_campaign_sessi_key ON public.typed_intent_command_receipts USING btree (origin_turn_id, campaign_session_id)`
- `typed_intent_command_receipts_turn_idx` — `CREATE INDEX typed_intent_command_receipts_turn_idx ON public.typed_intent_command_receipts USING btree (campaign_session_id, event_sequence) WHERE (state = 'committed'::text)`

</details>


## Private inspiration and consent

### `campaign_inspiration_allowed_sensitivities`

**Purpose.** Set of sensitivity codes the campaign has explicitly allowed for private inspiration.

**Access pattern.** Selection loads the full set by campaign and intersects it with source/grant sensitivity sets. Rows are replaced/inserted as part of safety setup and cascade through `campaign_inspiration_settings`.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:3263` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `sensitivity_code` | `text` | required; PK component; checked | Controlled sensitivity code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `campaign_inspiration_allowed_sensitivities_pkey` — `PRIMARY KEY (campaign_session_id, sensitivity_code)`
- `campaign_inspiration_allowed_sensitivi_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE`
- `campaign_inspiration_allowed_sensitiviti_sensitivity_code_check` — `CHECK (octet_length(sensitivity_code) >= 1 AND octet_length(sensitivity_code) <= 128 AND sensitivity_code ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `campaign_inspiration_excluded_participants`

**Purpose.** Campaign-specific participant IDs whose contributed sources must not be selected.

**Access pattern.** Selection scans by campaign and excludes sources connected through `private_inspiration_source_participants`. It is managed with safety setup and cascades with the settings row.

**Migration source(s).** `migrations/0017_private_inspiration_safety_setup.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:61` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `participant_id` | `text` | required; PK component; checked | Identifier for the associated participant; used to scope, join, or correlate this row. |

<details>
<summary>Exact table constraints</summary>

- `campaign_inspiration_excluded_participants_pkey` — `PRIMARY KEY (campaign_session_id, participant_id)`
- `campaign_inspiration_excluded_particip_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE`
- `campaign_inspiration_excluded_participants_participant_id_check` — `CHECK (participant_id ~ '^participant:[0-9a-f]{32}$'::text)`

</details>

### `campaign_inspiration_excluded_topics`

**Purpose.** Campaign-specific topic/safety codes excluded from private inspiration.

**Access pattern.** Selection scans by campaign and removes matching candidates. It is managed with safety setup and cascades with the settings row.

**Migration source(s).** `migrations/0017_private_inspiration_safety_setup.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:59` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `safety_code` | `text` | required; PK component; checked | Controlled safety code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `campaign_inspiration_excluded_topics_pkey` — `PRIMARY KEY (campaign_session_id, safety_code)`
- `campaign_inspiration_excluded_topics_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE`
- `campaign_inspiration_excluded_topics_safety_code_check` — `CHECK (octet_length(safety_code) >= 1 AND octet_length(safety_code) <= 128 AND safety_code ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `campaign_inspiration_lines`

**Purpose.** Hard “line” safety exclusions for a campaign.

**Access pattern.** Selection loads the set by campaign and rejects matching material. It is a compact composite-key child of campaign inspiration settings and is managed with the safety setup transaction.

**Migration source(s).** `migrations/0017_private_inspiration_safety_setup.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:55` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `safety_code` | `text` | required; PK component; checked | Controlled safety code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `campaign_inspiration_lines_pkey` — `PRIMARY KEY (campaign_session_id, safety_code)`
- `campaign_inspiration_lines_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE`
- `campaign_inspiration_lines_safety_code_check` — `CHECK (octet_length(safety_code) >= 1 AND octet_length(safety_code) <= 128 AND safety_code ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `campaign_inspiration_settings`

**Purpose.** One mutable private-inspiration policy and deterministic RNG cursor per campaign.

**Access pattern.** Setup and operator/player controls insert or update the campaign row under revision checks. Selection locks/reads it with its child allow/exclusion sets, advances `rng_cursor`, and respects local pause/global kill switches. Campaign deletion cascades all setup data.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`, `migrations/0014_private_inspiration_player_controls.sql`, `migrations/0017_private_inspiration_safety_setup.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:505` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `revision` | `bigint` | required; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `enabled` | `boolean` | required; default `false`; checked | Feature/row eligibility flag evaluated by runtime selection. |
| `safety_setup_complete` | `boolean` | required; default `false`; checked | Boolean flag indicating whether safety setup complete is true for this row. |
| `adults_only` | `boolean` | required; default `true`; checked | Boolean flag indicating whether adults only is true for this row. |
| `fictional_distance` | `text` | required; default `'high_locked'`; checked | Controlled privacy/safety setting requiring source material to be transformed away from real identities/events. |
| `audience` | `text` | required; default `'private_campaign'`; checked | Controlled audience scope required by campaign policy and consent. |
| `media` | `text` | required; default `'text'`; checked | Controlled output medium required by source and consent eligibility. |
| `q11_policy_id` | `text` | required; default `'q11_conservative_v1'`; checked | Pinned conservative screening policy identity used for source eligibility. |
| `safety_setup_evidence_digest` | `text` | nullable; checked | Deterministic digest of safety setup evidence, used for integrity/equality checks without retaining the raw input. |
| `safety_reviewer_id` | `text` | nullable; checked | Identifier for the associated safety reviewer; used to scope, join, or correlate this row. |
| `safety_reviewed_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when safety reviewed occurred. |
| `rng_cursor` | `bigint` | required; default `0`; checked | Durable deterministic RNG cursor locked and advanced by selection transactions. |
| `updated_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when updated occurred. |
| `generation_paused` | `boolean` | required; default `false` | Campaign-local pause flag checked before private-inspiration selection/generation. |
| `tone` | `text` | required; default `'gothic_adventure'`; checked | Controlled campaign inspiration tone used to constrain generation. |

<details>
<summary>Exact table constraints</summary>

- `campaign_inspiration_settings_pkey` — `PRIMARY KEY (campaign_session_id)`
- `campaign_inspiration_settings_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `campaign_inspiration_setting_safety_setup_evidence_digest_check` — `CHECK (safety_setup_evidence_digest IS NULL OR safety_setup_evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `campaign_inspiration_settings_adults_only_check` — `CHECK (adults_only)`
- `campaign_inspiration_settings_audience_check` — `CHECK (audience = 'private_campaign'::text)`
- `campaign_inspiration_settings_check` — `CHECK (safety_setup_complete AND safety_setup_evidence_digest IS NOT NULL AND safety_reviewer_id IS NOT NULL AND safety_reviewed_at_epoch IS NOT NULL OR NOT safety_setup_complete AND safety_setup_evidence_digest IS NULL AND safety_reviewer_id IS NULL AND safety_reviewed_at_epoch IS NULL)`
- `campaign_inspiration_settings_check1` — `CHECK (NOT enabled OR safety_setup_complete)`
- `campaign_inspiration_settings_fictional_distance_check` — `CHECK (fictional_distance = 'high_locked'::text)`
- `campaign_inspiration_settings_media_check` — `CHECK (media = 'text'::text)`
- `campaign_inspiration_settings_q11_policy_id_check` — `CHECK (q11_policy_id = 'q11_conservative_v1'::text)`
- `campaign_inspiration_settings_revision_check` — `CHECK (revision > 0)`
- `campaign_inspiration_settings_rng_cursor_check` — `CHECK (rng_cursor >= 0)`
- `campaign_inspiration_settings_safety_reviewed_at_epoch_check` — `CHECK (safety_reviewed_at_epoch >= 0)`
- `campaign_inspiration_settings_safety_reviewer_id_check` — `CHECK (safety_reviewer_id IS NULL OR safety_reviewer_id ~ '^operator:[0-9a-f]{32}$'::text)`
- `campaign_inspiration_settings_schema_version_check` — `CHECK (schema_version = 1)`
- `campaign_inspiration_settings_tone_check` — `CHECK (tone = ANY (ARRAY['gothic_adventure'::text, 'hopeful_adventure'::text, 'lighthearted_adventure'::text]))`
- `campaign_inspiration_settings_updated_at_epoch_check` — `CHECK (updated_at_epoch >= 0)`

</details>

### `campaign_inspiration_veils`

**Purpose.** “Veil” safety codes requiring off-screen or non-explicit treatment.

**Access pattern.** Selection loads the set by campaign to constrain transformation/presentation. It is managed as a composite-key child of campaign inspiration settings.

**Migration source(s).** `migrations/0017_private_inspiration_safety_setup.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:57` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `safety_code` | `text` | required; PK component; checked | Controlled safety code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `campaign_inspiration_veils_pkey` — `PRIMARY KEY (campaign_session_id, safety_code)`
- `campaign_inspiration_veils_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_inspiration_settings(campaign_session_id) ON DELETE CASCADE`
- `campaign_inspiration_veils_safety_code_check` — `CHECK (octet_length(safety_code) >= 1 AND octet_length(safety_code) <= 128 AND safety_code ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `private_inspiration_command_receipts`

**Purpose.** Campaign-scoped idempotency receipts for private-inspiration setup, controls, consent, selection, and deletion operations.

**Access pattern.** Each command probes `(campaign_session_id, idempotency_key)`, validates operation/fingerprint, and inserts a replay response in the same transaction as its effects. Rows are immutable and cascade with the campaign.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`, `migrations/0014_private_inspiration_player_controls.sql`, `migrations/0016_private_inspiration_interventions.sql`, `migrations/0020_private_inspiration_participant_deletion.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:3815` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `campaign_session_id` | `text` | required; PK component; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `idempotency_key` | `text` | required; PK component; checked | Opaque client/operator retry key within the table’s documented scope. |
| `operation_code` | `text` | required; checked | Controlled private-inspiration operation discriminator. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `response_json` | `text` | required; checked | Bounded serialized response replayed for an exact duplicate command. |
| `created_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when created occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_command_receipts_pkey` — `PRIMARY KEY (campaign_session_id, idempotency_key)`
- `private_inspiration_command_receipts_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_command_receipts_created_at_epoch_check` — `CHECK (created_at_epoch >= 0)`
- `private_inspiration_command_receipts_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_command_receipts_operation_code_check` — `CHECK (operation_code = ANY (ARRAY['settings_change'::text, 'settings_pause'::text, 'settings_disable'::text, 'source_register'::text, 'source_review'::text, 'participant_verify'::text, 'participant_revoke'::text, 'participant_delete'::text, 'consent_grant'::text, 'consent_revoke'::text, 'veto_apply'::text, 'derived_work_register'::text, 'presentation_control'::text]))`
- `private_inspiration_command_receipts_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_command_receipts_response_json_check` — `CHECK (octet_length(response_json) >= 2 AND octet_length(response_json) <= 65536 AND jsonb_typeof(response_json::jsonb) = 'object'::text)`

</details>

### `private_inspiration_consent_grants`

**Purpose.** Versioned participant consent grant tying one campaign, source version, audience/media/transformation, artifact policy, evidence, state, and expiry together.

**Access pattern.** Grant/revoke workflows insert or update under operator and participant evidence checks. Candidate selection joins active, unexpired grants by campaign/source/participant/scope; a partial unique index prevents overlapping active grant scope. Revocation is a state/timestamp update, not deletion.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:590` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `grant_id` | `text` | required; PK; checked | Identifier for the associated grant; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `source_id` | `text` | required; FK → private_inspiration_source_media(source_id, source_version, media); FK → private_inspiration_source_participants(source_id, source_version, participant_id); FK → private_inspiration_sources(source_id, source_version, source_digest) | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; FK → private_inspiration_source_media(source_id, source_version, media); FK → private_inspiration_source_participants(source_id, source_version, participant_id); FK → private_inspiration_sources(source_id, source_version, source_digest) | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `source_digest` | `text` | required; FK → private_inspiration_sources(source_id, source_version, source_digest) | Deterministic digest of source, used for integrity/equality checks without retaining the raw input. |
| `participant_id` | `text` | required; FK → private_inspiration_source_participants(source_id, source_version, participant_id) | Identifier for the associated participant; used to scope, join, or correlate this row. |
| `audience` | `text` | required; checked | Controlled audience scope required by campaign policy and consent. |
| `media` | `text` | required; FK → private_inspiration_source_media(source_id, source_version, media); checked | Controlled output medium required by source and consent eligibility. |
| `transformation` | `text` | required; checked | Controlled transformation permission granted for private source material. |
| `artifact_policy` | `text` | required; checked | Controlled retention/presentation policy for work derived from private material. |
| `reviewer_id` | `text` | required; checked | Identifier for the associated reviewer; used to scope, join, or correlate this row. |
| `participant_confirmation_digest` | `text` | required; checked | Deterministic digest of participant confirmation, used for integrity/equality checks without retaining the raw input. |
| `review_evidence_digest` | `text` | required; checked | Deterministic digest of review evidence, used for integrity/equality checks without retaining the raw input. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `granted_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when granted occurred. |
| `expires_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when expires occurred. |
| `revoked_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when revoked occurred. |
| `revocation_code` | `text` | nullable; checked | Controlled revocation code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_consent_grants_pkey` — `PRIMARY KEY (grant_id)`
- `private_inspiration_consent_g_source_id_source_version_med_fkey` — `FOREIGN KEY (source_id, source_version, media) REFERENCES private_inspiration_source_media(source_id, source_version, media)`
- `private_inspiration_consent_g_source_id_source_version_par_fkey` — `FOREIGN KEY (source_id, source_version, participant_id) REFERENCES private_inspiration_source_participants(source_id, source_version, participant_id)`
- `private_inspiration_consent_g_source_id_source_version_sou_fkey` — `FOREIGN KEY (source_id, source_version, source_digest) REFERENCES private_inspiration_sources(source_id, source_version, source_digest)`
- `private_inspiration_consent_grants_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_consent__participant_confirmation_dig_check` — `CHECK (participant_confirmation_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_consent_grants_artifact_policy_check` — `CHECK (artifact_policy = ANY (ARRAY['delete_derived'::text, 'redact_derived'::text, 'retain_minimal_audit'::text]))`
- `private_inspiration_consent_grants_audience_check` — `CHECK (audience = 'private_campaign'::text)`
- `private_inspiration_consent_grants_check` — `CHECK (expires_at_epoch > granted_at_epoch)`
- `private_inspiration_consent_grants_check1` — `CHECK (revoked_at_epoch >= granted_at_epoch)`
- `private_inspiration_consent_grants_check2` — `CHECK ((state = ANY (ARRAY['active'::text, 'expired'::text])) AND revoked_at_epoch IS NULL AND revocation_code IS NULL OR state = 'revoked'::text AND revoked_at_epoch IS NOT NULL AND revocation_code IS NOT NULL)`
- `private_inspiration_consent_grants_grant_id_check` — `CHECK (octet_length(grant_id) >= 1 AND octet_length(grant_id) <= 128 AND grant_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_consent_grants_granted_at_epoch_check` — `CHECK (granted_at_epoch >= 0)`
- `private_inspiration_consent_grants_media_check` — `CHECK (media = ANY (ARRAY['text'::text, 'image'::text, 'recap'::text]))`
- `private_inspiration_consent_grants_review_evidence_digest_check` — `CHECK (review_evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_consent_grants_reviewer_id_check` — `CHECK (reviewer_id ~ '^operator:[0-9a-f]{32}$'::text)`
- `private_inspiration_consent_grants_revocation_code_check` — `CHECK (revocation_code IS NULL OR (revocation_code = ANY (ARRAY['participant_revoked'::text, 'reviewer_revoked'::text, 'source_changed'::text, 'campaign_disabled'::text, 'privacy_request'::text])))`
- `private_inspiration_consent_grants_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_consent_grants_state_check` — `CHECK (state = ANY (ARRAY['active'::text, 'expired'::text, 'revoked'::text]))`
- `private_inspiration_consent_grants_transformation_check` — `CHECK (transformation = 'high_fiction_distance_v1'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_one_active_grant_scope_idx` — `CREATE UNIQUE INDEX private_inspiration_one_active_grant_scope_idx ON public.private_inspiration_consent_grants USING btree (campaign_session_id, source_id, source_version, participant_id, audience, media, transformation) WHERE (state = 'active'::text)`

</details>

### `private_inspiration_consent_sensitivities`

**Purpose.** Sensitivity codes explicitly covered by one consent grant.

**Access pattern.** Grant creation inserts the set; candidate selection joins it to source and campaign safety codes. Rows cascade when the parent grant is removed.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:2087` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `grant_id` | `text` | required; PK component; FK → private_inspiration_consent_grants(grant_id) ON DELETE CASCADE | Identifier for the associated grant; used to scope, join, or correlate this row. |
| `sensitivity_code` | `text` | required; PK component; checked | Controlled sensitivity code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_consent_sensitivities_pkey` — `PRIMARY KEY (grant_id, sensitivity_code)`
- `private_inspiration_consent_sensitivities_grant_id_fkey` — `FOREIGN KEY (grant_id) REFERENCES private_inspiration_consent_grants(grant_id) ON DELETE CASCADE`
- `private_inspiration_consent_sensitivitie_sensitivity_code_check` — `CHECK (octet_length(sensitivity_code) >= 1 AND octet_length(sensitivity_code) <= 128 AND sensitivity_code ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `private_inspiration_deletion_tombstones`

**Purpose.** Delayed-deletion marker for participant-linked private-inspiration data.

**Access pattern.** Participant deletion first removes protected source bodies, then revokes grants, quarantines sources, cancels/redacts derived work, marks verification deleted, and inserts this tombstone. Opaque participant/source/grant/audit metadata is intentionally retained; purging at the deadline removes the tombstone, not all relational history. Lookups by participant prevent duplicate/inconsistent requests.

**Migration source(s).** `migrations/0020_private_inspiration_participant_deletion.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:410` (DELETE/INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `participant_id` | `text` | required; PK; checked | Identifier for the associated participant; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `requested_by_operator_id` | `text` | required; checked | Identifier for the associated requested by operator; used to scope, join, or correlate this row. |
| `deletion_evidence_digest` | `text` | required; checked | Deterministic digest of deletion evidence, used for integrity/equality checks without retaining the raw input. |
| `requested_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when requested occurred. |
| `delete_after_epoch` | `bigint` | required; checked | UTC Unix epoch value associated with delete after. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_deletion_tombstones_pkey` — `PRIMARY KEY (participant_id)`
- `private_inspiration_deletion_tom_deletion_evidence_digest_check` — `CHECK (deletion_evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_deletion_tom_requested_by_operator_id_check` — `CHECK (requested_by_operator_id ~ '^operator:[0-9a-f]{32}$'::text)`
- `private_inspiration_deletion_tombstone_requested_at_epoch_check` — `CHECK (requested_at_epoch >= 0)`
- `private_inspiration_deletion_tombstones_check` — `CHECK (delete_after_epoch = (requested_at_epoch + 3024000))`
- `private_inspiration_deletion_tombstones_participant_id_check` — `CHECK (participant_id ~ '^participant:[0-9a-f]{32}$'::text)`
- `private_inspiration_deletion_tombstones_schema_version_check` — `CHECK (schema_version = 1)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_deletion_tombstone_expiry_idx` — `CREATE INDEX private_inspiration_deletion_tombstone_expiry_idx ON public.private_inspiration_deletion_tombstones USING btree (delete_after_epoch, participant_id)`

</details>

### `private_inspiration_derived_work`

**Purpose.** Durable state machine for generated work derived from an approved private-inspiration selection.

**Access pattern.** Selection inserts pending work; cancellation/privacy controls update state; text publication locks the row and marks it completed with artifact/output evidence. Reads are campaign/work/selection scoped, and indexes support pending work and completed artifact lookup.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`, `migrations/0015_private_inspiration_presentations.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:657` (INSERT/SELECT/UPDATE), `crates/game-server/src/repository/presentations.rs:342` (SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `work_id` | `text` | required; PK; checked | Identifier for the associated work; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `selection_id` | `text` | required; FK → private_inspiration_selection_audits(selection_id) ON DELETE CASCADE | Identifier for the associated selection; used to scope, join, or correlate this row. |
| `source_id` | `text` | required; FK → private_inspiration_sources(source_id, source_version, source_digest); FK → private_inspiration_source_media(source_id, source_version, media) | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; FK → private_inspiration_sources(source_id, source_version, source_digest); FK → private_inspiration_source_media(source_id, source_version, media) | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `source_digest` | `text` | required; FK → private_inspiration_sources(source_id, source_version, source_digest) | Deterministic digest of source, used for integrity/equality checks without retaining the raw input. |
| `work_kind` | `text` | required; FK → private_inspiration_source_media(source_id, source_version, media); checked | Controlled generated-work modality/type. |
| `state` | `text` | required; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `artifact_policy` | `text` | required; checked | Controlled retention/presentation policy for work derived from private material. |
| `created_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when created occurred. |
| `cancellation_requested_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when cancellation requested occurred. |
| `completed_artifact_id` | `text` | nullable; checked | Identifier for the associated completed artifact; used to scope, join, or correlate this row. |
| `completed_output_digest` | `text` | nullable; checked | Deterministic digest of completed output, used for integrity/equality checks without retaining the raw input. |
| `completed_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when completed occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_derived_work_pkey` — `PRIMARY KEY (work_id)`
- `private_inspiration_derived_w_source_id_source_version_sou_fkey` — `FOREIGN KEY (source_id, source_version, source_digest) REFERENCES private_inspiration_sources(source_id, source_version, source_digest)`
- `private_inspiration_derived_w_source_id_source_version_wor_fkey` — `FOREIGN KEY (source_id, source_version, work_kind) REFERENCES private_inspiration_source_media(source_id, source_version, media)`
- `private_inspiration_derived_work_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_derived_work_selection_id_fkey` — `FOREIGN KEY (selection_id) REFERENCES private_inspiration_selection_audits(selection_id) ON DELETE CASCADE`
- `private_inspiration_derived_work_artifact_policy_check` — `CHECK (artifact_policy = ANY (ARRAY['delete_derived'::text, 'redact_derived'::text, 'retain_minimal_audit'::text]))`
- `private_inspiration_derived_work_check` — `CHECK (cancellation_requested_at_epoch >= created_at_epoch)`
- `private_inspiration_derived_work_check1` — `CHECK (state = 'cancellation_requested'::text AND cancellation_requested_at_epoch IS NOT NULL OR state <> 'cancellation_requested'::text AND cancellation_requested_at_epoch IS NULL)`
- `private_inspiration_derived_work_check2` — `CHECK (completed_at_epoch IS NULL OR completed_at_epoch >= created_at_epoch)`
- `private_inspiration_derived_work_completed_artifact_id_check` — `CHECK (completed_artifact_id IS NULL OR octet_length(completed_artifact_id) >= 1 AND octet_length(completed_artifact_id) <= 128 AND completed_artifact_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_derived_work_completed_output_digest_check` — `CHECK (completed_output_digest IS NULL OR completed_output_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_derived_work_completion_check` — `CHECK ((state = ANY (ARRAY['pending'::text, 'cancellation_requested'::text])) AND completed_artifact_id IS NULL AND completed_output_digest IS NULL AND completed_at_epoch IS NULL OR (state = ANY (ARRAY['completed'::text, 'redacted'::text])) AND completed_artifact_id IS NOT NULL AND completed_output_digest IS NOT NULL AND completed_at_epoch IS NOT NULL OR state = 'deleted'::text AND completed_artifact_id IS NULL AND completed_output_digest IS NOT NULL AND completed_at_epoch IS NOT NULL)`
- `private_inspiration_derived_work_created_at_epoch_check` — `CHECK (created_at_epoch >= 0)`
- `private_inspiration_derived_work_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_derived_work_state_check` — `CHECK (state = ANY (ARRAY['pending'::text, 'cancellation_requested'::text, 'completed'::text, 'redacted'::text, 'deleted'::text]))`
- `private_inspiration_derived_work_work_id_check` — `CHECK (octet_length(work_id) >= 1 AND octet_length(work_id) <= 128 AND work_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_derived_work_work_kind_check` — `CHECK (work_kind = ANY (ARRAY['text'::text, 'image'::text, 'recap'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_completed_work_idx` — `CREATE INDEX private_inspiration_completed_work_idx ON public.private_inspiration_derived_work USING btree (campaign_session_id, source_id, source_version, state) WHERE (state = ANY (ARRAY['completed'::text, 'redacted'::text]))`
- `private_inspiration_pending_work_idx` — `CREATE INDEX private_inspiration_pending_work_idx ON public.private_inspiration_derived_work USING btree (campaign_session_id, source_id, source_version, state) WHERE (state = 'pending'::text)`

</details>

### `private_inspiration_global_command_receipts`

**Purpose.** Global-scope idempotency receipts for private-inspiration kill-switch commands.

**Access pattern.** Operator commands probe by key, compare the request fingerprint, and insert the serialized response once. Rows are immutable and intentionally not campaign-scoped.

**Migration source(s).** `migrations/0019_private_inspiration_global_kill_switch.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:300` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `idempotency_key` | `text` | required; PK; checked | Opaque client/operator retry key within the table’s documented scope. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `response_json` | `text` | required; checked | Bounded serialized response replayed for an exact duplicate command. |
| `created_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when created occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_global_command_receipts_pkey` — `PRIMARY KEY (idempotency_key)`
- `private_inspiration_global_command_re_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_global_command_recei_created_at_epoch_check` — `CHECK (created_at_epoch >= 0)`
- `private_inspiration_global_command_receip_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_global_command_receipts_response_json_check` — `CHECK (octet_length(response_json) >= 2 AND octet_length(response_json) <= 65536 AND jsonb_typeof(response_json::jsonb) = 'object'::text)`

</details>

### `private_inspiration_global_control`

**Purpose.** Singleton global kill switch for all private-inspiration generation.

**Access pattern.** Runtime selection/publication reads the singleton, sometimes `FOR SHARE`; offline/operator control locks and updates revision, disabled flag, operator, evidence, and epoch. A CHECK enforces exactly one row.

**Migration source(s).** `migrations/0019_private_inspiration_global_kill_switch.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:186` (SELECT/UPDATE), `crates/game-server/src/repository/presentations.rs:326` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `singleton` | `boolean` | required; default `true`; PK; checked | Boolean flag indicating whether singleton is true for this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `revision` | `bigint` | required; checked | Optimistic concurrency revision; mutating workflows compare and increment it. |
| `generation_disabled` | `boolean` | required; default `false`; checked | Global fail-closed kill-switch flag for private-inspiration generation. |
| `operator_id` | `text` | nullable; checked | Identifier for the associated operator; used to scope, join, or correlate this row. |
| `evidence_digest` | `text` | nullable; checked | Deterministic digest of evidence, used for integrity/equality checks without retaining the raw input. |
| `updated_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when updated occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_global_control_pkey` — `PRIMARY KEY (singleton)`
- `private_inspiration_global_control_check` — `CHECK (generation_disabled AND operator_id IS NOT NULL AND evidence_digest IS NOT NULL OR NOT generation_disabled)`
- `private_inspiration_global_control_evidence_digest_check` — `CHECK (evidence_digest IS NULL OR evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_global_control_operator_id_check` — `CHECK (operator_id IS NULL OR operator_id ~ '^operator:[0-9a-f]{32}$'::text)`
- `private_inspiration_global_control_revision_check` — `CHECK (revision > 0)`
- `private_inspiration_global_control_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_global_control_singleton_check` — `CHECK (singleton)`
- `private_inspiration_global_control_updated_at_epoch_check` — `CHECK (updated_at_epoch >= 0)`

</details>

### `private_inspiration_participants`

**Purpose.** Verified participant registry for consentable private source material.

**Access pattern.** Offline/operator registration inserts versioned verification evidence; revocation updates the row. Consent/source/veto workflows resolve by `participant_id`. The schema stores opaque IDs and evidence digests rather than contact details.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:1008` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `participant_id` | `text` | required; PK; checked | Identifier for the associated participant; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `verification_state` | `text` | required; checked | Controlled participant verification lifecycle state. |
| `verification_method` | `text` | required; checked | Controlled out-of-band participant verification method. |
| `verification_evidence_digest` | `text` | required; checked | Deterministic digest of verification evidence, used for integrity/equality checks without retaining the raw input. |
| `verifier_id` | `text` | required; checked | Identifier for the associated verifier; used to scope, join, or correlate this row. |
| `verified_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when verified occurred. |
| `revoked_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when revoked occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_participants_pkey` — `PRIMARY KEY (participant_id)`
- `private_inspiration_particip_verification_evidence_digest_check` — `CHECK (verification_evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_participants_check` — `CHECK (revoked_at_epoch >= verified_at_epoch)`
- `private_inspiration_participants_check1` — `CHECK (verification_state = 'verified'::text AND revoked_at_epoch IS NULL OR verification_state = 'revoked'::text AND revoked_at_epoch IS NOT NULL)`
- `private_inspiration_participants_participant_id_check` — `CHECK (participant_id ~ '^participant:[0-9a-f]{32}$'::text)`
- `private_inspiration_participants_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_participants_verification_method_check` — `CHECK (verification_method = ANY (ARRAY['participant_signed_confirmation'::text, 'timestamped_two_channel_acknowledgement'::text]))`
- `private_inspiration_participants_verification_state_check` — `CHECK (verification_state = ANY (ARRAY['verified'::text, 'revoked'::text]))`
- `private_inspiration_participants_verified_at_epoch_check` — `CHECK (verified_at_epoch >= 0)`
- `private_inspiration_participants_verifier_id_check` — `CHECK (verifier_id ~ '^operator:[0-9a-f]{32}$'::text)`

</details>

### `private_inspiration_privacy_audits`

**Purpose.** Append-only minimized privacy/audit event stream for private-inspiration operations.

**Access pattern.** Consent, veto, deletion, selection, derived-work, and publication paths insert result-coded events. Ordinary runtime paths do not update rows; campaign may be null for global/participant operations, allowing evidence to survive scoped deletion without storing source text.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`, `migrations/0015_private_inspiration_presentations.sql`, `migrations/0016_private_inspiration_interventions.sql`, `migrations/0019_private_inspiration_global_kill_switch.sql`, `migrations/0020_private_inspiration_participant_deletion.sql`, `migrations/0022_private_inspiration_runtime_and_access.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:3879` (INSERT), `crates/game-server/src/repository/presentations.rs:495` (INSERT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `audit_id` | `text` | required; PK; checked | Identifier for the associated audit; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `campaign_session_id` | `text` | nullable; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `operation_code` | `text` | required; checked | Controlled private-inspiration operation discriminator. |
| `subject_kind` | `text` | required; checked | Controlled subject kind discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `subject_id` | `text` | required; checked | Identifier for the associated subject; used to scope, join, or correlate this row. |
| `secondary_id` | `text` | nullable; checked | Identifier for the associated secondary; used to scope, join, or correlate this row. |
| `result_code` | `text` | required; checked | Minimized controlled outcome code for audit/restricted-access reporting. |
| `occurred_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when occurred occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_privacy_audits_pkey` — `PRIMARY KEY (audit_id)`
- `private_inspiration_privacy_audits_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_privacy_audits_audit_id_check` — `CHECK (octet_length(audit_id) >= 1 AND octet_length(audit_id) <= 128 AND audit_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_privacy_audits_occurred_at_epoch_check` — `CHECK (occurred_at_epoch >= 0)`
- `private_inspiration_privacy_audits_operation_code_check` — `CHECK (operation_code = ANY (ARRAY['settings_changed'::text, 'source_registered'::text, 'source_reviewed'::text, 'source_quarantined'::text, 'participant_verified'::text, 'participant_revoked'::text, 'participant_deletion_requested'::text, 'deletion_tombstone_expired'::text, 'consent_granted'::text, 'consent_revoked'::text, 'veto_applied'::text, 'selection_reserved'::text, 'derived_work_registered'::text, 'derived_work_completed'::text, 'derived_work_cancel_requested'::text, 'derived_work_redacted'::text, 'derived_work_deleted'::text, 'presentation_veiled'::text, 'owner_veto_applied'::text, 'privacy_reported'::text, 'global_kill_switch'::text, 'restricted_diagnostic_access'::text]))`
- `private_inspiration_privacy_audits_result_code_check` — `CHECK (result_code = ANY (ARRAY['applied'::text, 'replayed'::text, 'denied'::text, 'cancel_requested'::text]))`
- `private_inspiration_privacy_audits_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_privacy_audits_secondary_id_check` — `CHECK (secondary_id IS NULL OR octet_length(secondary_id) >= 1 AND octet_length(secondary_id) <= 128 AND secondary_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_privacy_audits_subject_id_check` — `CHECK (octet_length(subject_id) >= 1 AND octet_length(subject_id) <= 128 AND subject_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_privacy_audits_subject_kind_check` — `CHECK (subject_kind = ANY (ARRAY['campaign'::text, 'source_version'::text, 'participant'::text, 'consent_grant'::text, 'veto'::text, 'selection'::text, 'derived_work'::text, 'restricted_diagnostic'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_privacy_audit_idx` — `CREATE INDEX private_inspiration_privacy_audit_idx ON public.private_inspiration_privacy_audits USING btree (campaign_session_id, occurred_at_epoch DESC, audit_id)`

</details>

### `private_inspiration_restricted_access_audits`

**Purpose.** Append-only evidence for restricted operator access to protected private-inspiration material or exports.

**Access pattern.** Restricted tools insert an idempotent, fingerprinted result record and query it for exact replay/history. The row contains only IDs/digests/purpose/result, not protected content; campaign deletion sets the campaign ID null.

**Migration source(s).** `migrations/0022_private_inspiration_runtime_and_access.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:205` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `audit_id` | `text` | required; PK; checked | Identifier for the associated audit; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `idempotency_key` | `text` | required; unique; checked | Opaque client/operator retry key within the table’s documented scope. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `campaign_session_id` | `text` | nullable; FK → campaign_sessions(id) ON DELETE SET NULL | Campaign that owns/scopes the row and is the principal partition key for access. |
| `operator_id` | `text` | required; checked | Identifier for the associated operator; used to scope, join, or correlate this row. |
| `access_kind` | `text` | required; checked | Controlled access kind discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `purpose_code` | `text` | required; checked | Controlled purpose code discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `subject_id` | `text` | required; checked | Identifier for the associated subject; used to scope, join, or correlate this row. |
| `evidence_digest` | `text` | required; checked | Deterministic digest of evidence, used for integrity/equality checks without retaining the raw input. |
| `result_code` | `text` | required; checked | Minimized controlled outcome code for audit/restricted-access reporting. |
| `occurred_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when occurred occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_restricted_access_audits_pkey` — `PRIMARY KEY (audit_id)`
- `private_inspiration_restricted_access_audit_idempotency_key_key` — `UNIQUE (idempotency_key)`
- `private_inspiration_restricted_access__campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE SET NULL`
- `private_inspiration_restricted_access_a_occurred_at_epoch_check` — `CHECK (occurred_at_epoch >= 0)`
- `private_inspiration_restricted_access_aud_evidence_digest_check` — `CHECK (evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_restricted_access_aud_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_restricted_access_audi_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_restricted_access_audits_access_kind_check` — `CHECK (access_kind = ANY (ARRAY['source_plaintext'::text, 'source_backup'::text, 'image_quarantine'::text, 'generation_diagnostic'::text]))`
- `private_inspiration_restricted_access_audits_audit_id_check` — `CHECK (octet_length(audit_id) >= 1 AND octet_length(audit_id) <= 128 AND audit_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_restricted_access_audits_operator_id_check` — `CHECK (operator_id ~ '^operator:[0-9a-f]{32}$'::text)`
- `private_inspiration_restricted_access_audits_purpose_code_check` — `CHECK (purpose_code = ANY (ARRAY['source_review'::text, 'data_rights_request'::text, 'incident_response'::text, 'restore_drill'::text, 'security_validation'::text]))`
- `private_inspiration_restricted_access_audits_result_code_check` — `CHECK (result_code = ANY (ARRAY['allowed'::text, 'denied'::text]))`
- `private_inspiration_restricted_access_audits_subject_id_check` — `CHECK (octet_length(subject_id) >= 1 AND octet_length(subject_id) <= 128 AND subject_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_restricted_access_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_restricted_access_audit_idempotency_key_key` — `CREATE UNIQUE INDEX private_inspiration_restricted_access_audit_idempotency_key_key ON public.private_inspiration_restricted_access_audits USING btree (idempotency_key)`
- `private_inspiration_restricted_access_time_idx` — `CREATE INDEX private_inspiration_restricted_access_time_idx ON public.private_inspiration_restricted_access_audits USING btree (occurred_at_epoch DESC, audit_id)`

</details>

### `private_inspiration_runtime_facts`

**Purpose.** Ordered neutral facts belonging to a sanitized runtime prompt.

**Access pattern.** Offline approval inserts the bounded fact list. Runtime selection reads all facts by `(source_id, source_version)` ordered by `fact_index` to reconstruct a minimized prompt. Rows cascade with the runtime prompt.

**Migration source(s).** `migrations/0022_private_inspiration_runtime_and_access.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:121` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; FK → private_inspiration_runtime_prompts(source_id, source_version) ON DELETE CASCADE | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; FK → private_inspiration_runtime_prompts(source_id, source_version) ON DELETE CASCADE | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `fact_index` | `smallint` | required; PK component; checked | Stable zero/one-based ordering key for neutral runtime facts (as constrained). |
| `neutral_fact` | `text` | required; checked | Sanitized fictional/neutral fact safe to cross into ordinary runtime storage. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_runtime_facts_pkey` — `PRIMARY KEY (source_id, source_version, fact_index)`
- `private_inspiration_runtime_facts_source_id_source_version_fkey` — `FOREIGN KEY (source_id, source_version) REFERENCES private_inspiration_runtime_prompts(source_id, source_version) ON DELETE CASCADE`
- `private_inspiration_runtime_facts_fact_index_check` — `CHECK (fact_index >= 1 AND fact_index <= 4)`
- `private_inspiration_runtime_facts_neutral_fact_check` — `CHECK (neutral_fact = btrim(neutral_fact) AND char_length(neutral_fact) >= 1 AND char_length(neutral_fact) <= 240 AND neutral_fact !~ '[[:cntrl:]]'::text)`

</details>

### `private_inspiration_runtime_prompts`

**Purpose.** Sanitized runtime projection of a reviewed source version, containing only selection metadata safe for the ordinary server.

**Access pattern.** Offline approval inserts/upserts the projection. Runtime selection reads enabled rows and joins facts plus consent/safety mappings. The protected source mount is not needed by the normal server; projection digest detects drift.

**Migration source(s).** `migrations/0022_private_inspiration_runtime_and_access.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:99` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; composite unique; FK → private_inspiration_sources(source_id, source_version, source_digest) ON DELETE CASCADE | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; composite unique; FK → private_inspiration_sources(source_id, source_version, source_digest) ON DELETE CASCADE | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `source_digest` | `text` | required; composite unique; FK → private_inspiration_sources(source_id, source_version, source_digest) ON DELETE CASCADE | Deterministic digest of source, used for integrity/equality checks without retaining the raw input. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `selection_weight_nanounits` | `bigint` | required; checked | Integer fixed-point selection weight; avoids floating-point nondeterminism. |
| `minimum_level` | `smallint` | required; checked | Minimum runtime hero level at which the prompt is eligible. |
| `maximum_level` | `smallint` | nullable; checked | Optional maximum runtime hero level for prompt eligibility. |
| `cooldown_turns` | `bigint` | required; checked | Number of turns that must pass before reusing the source version. |
| `enabled` | `boolean` | required; checked | Feature/row eligibility flag evaluated by runtime selection. |
| `projection_digest` | `text` | required; checked | Deterministic digest of projection, used for integrity/equality checks without retaining the raw input. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_runtime_prompts_pkey` — `PRIMARY KEY (source_id, source_version)`
- `private_inspiration_runtime_p_source_id_source_version_sour_key` — `UNIQUE (source_id, source_version, source_digest)`
- `private_inspiration_runtime_p_source_id_source_version_sou_fkey` — `FOREIGN KEY (source_id, source_version, source_digest) REFERENCES private_inspiration_sources(source_id, source_version, source_digest) ON DELETE CASCADE`
- `private_inspiration_runtime_pr_selection_weight_nanounits_check` — `CHECK (selection_weight_nanounits >= 1 AND selection_weight_nanounits <= '1000000000000000'::bigint)`
- `private_inspiration_runtime_prompts_check` — `CHECK (maximum_level IS NULL OR maximum_level >= minimum_level AND maximum_level <= 20)`
- `private_inspiration_runtime_prompts_check1` — `CHECK (NOT enabled OR cooldown_turns > 0)`
- `private_inspiration_runtime_prompts_cooldown_turns_check` — `CHECK (cooldown_turns >= 0 AND cooldown_turns <= 1000000)`
- `private_inspiration_runtime_prompts_minimum_level_check` — `CHECK (minimum_level >= 1 AND minimum_level <= 20)`
- `private_inspiration_runtime_prompts_projection_digest_check` — `CHECK (projection_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_runtime_prompts_schema_version_check` — `CHECK (schema_version = 1)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_runtime_p_source_id_source_version_sour_key` — `CREATE UNIQUE INDEX private_inspiration_runtime_p_source_id_source_version_sour_key ON public.private_inspiration_runtime_prompts USING btree (source_id, source_version, source_digest)`

</details>

### `private_inspiration_selection_audits`

**Purpose.** Append-only deterministic-selection proof for each private inspiration trigger, including eligible-set digest, sample math, cursor movement, and selected/no-selection outcome.

**Access pattern.** Selection first checks the campaign/key idempotency path, computes eligibility, inserts this audit, advances settings cursor, records usage, and optionally creates derived work in one transaction. Replays load by campaign/key; presentations lock the selected row before completing work.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`, `migrations/0014_private_inspiration_player_controls.sql`, `migrations/0019_private_inspiration_global_kill_switch.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:2393` (INSERT/SELECT), `crates/game-server/src/repository/presentations.rs:343` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `selection_id` | `text` | required; PK; checked | Identifier for the associated selection; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `campaign_session_id` | `text` | required; composite unique; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `idempotency_key` | `text` | required; composite unique; checked | Opaque client/operator retry key within the table’s documented scope. |
| `request_fingerprint` | `text` | required; checked | Digest of canonical command inputs; an idempotency-key replay must match it exactly. |
| `trigger_window_id` | `text` | required; checked | Identifier for the associated trigger window; used to scope, join, or correlate this row. |
| `campaign_revision` | `bigint` | required; checked | Campaign aggregate revision captured or produced by this operation. |
| `turn_number` | `bigint` | required; checked | Campaign/play-session turn ordinal used for ordering and cooldown/history queries. |
| `audience` | `text` | required; checked | Controlled audience scope required by campaign policy and consent. |
| `media` | `text` | required; FK → private_inspiration_source_media(source_id, source_version, media); checked | Controlled output medium required by source and consent eligibility. |
| `seed_reference` | `text` | required; checked | Opaque reference to the deterministic seed material; records reproducibility without storing secret/raw seed input. |
| `eligible_set_digest` | `text` | required; checked | Digest of the exact ordered eligible source set, making selection reproducible/auditable. |
| `eligible_source_count` | `bigint` | required; checked | Number of source versions in the exact eligible set. |
| `selected_source_id` | `text` | nullable; FK → private_inspiration_source_media(source_id, source_version, media); FK → private_inspiration_sources(source_id, source_version, source_digest); checked | Identifier for the associated selected source; used to scope, join, or correlate this row. |
| `selected_source_version` | `bigint` | nullable; FK → private_inspiration_source_media(source_id, source_version, media); FK → private_inspiration_sources(source_id, source_version, source_digest); checked | Version of the selected source; null when the audit records a no-selection outcome. |
| `selected_source_digest` | `text` | nullable; FK → private_inspiration_sources(source_id, source_version, source_digest); checked | Deterministic digest of selected source, used for integrity/equality checks without retaining the raw input. |
| `no_selection_reason` | `text` | nullable; checked | Controlled reason why no source was selected; mutually constrained with selected-source/sample fields. |
| `sample_numerator` | `bigint` | nullable; checked | Recorded deterministic sample numerator when a source was sampled; null for no-selection outcomes. |
| `sample_denominator` | `bigint` | nullable; checked | Recorded deterministic sample denominator used with `sample_numerator`. |
| `algorithm` | `text` | required; checked | Named deterministic sampling algorithm used for this selection proof. |
| `cursor_before` | `bigint` | required; checked | Campaign RNG cursor before the deterministic selection transaction. |
| `cursor_after` | `bigint` | required; checked | Campaign RNG cursor after the deterministic selection transaction. |
| `created_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when created occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_selection_audits_pkey` — `PRIMARY KEY (selection_id)`
- `private_inspiration_selection_campaign_session_id_idempoten_key` — `UNIQUE (campaign_session_id, idempotency_key)`
- `private_inspiration_selectio_selected_source_id_selected__fkey1` — `FOREIGN KEY (selected_source_id, selected_source_version, media) REFERENCES private_inspiration_source_media(source_id, source_version, media)`
- `private_inspiration_selection_audits_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_selection_selected_source_id_selected__fkey` — `FOREIGN KEY (selected_source_id, selected_source_version, selected_source_digest) REFERENCES private_inspiration_sources(source_id, source_version, source_digest)`
- `private_inspiration_selection_audit_eligible_source_count_check` — `CHECK (eligible_source_count >= 0)`
- `private_inspiration_selection_audits_algorithm_check` — `CHECK (algorithm = 'chacha20-v1'::text)`
- `private_inspiration_selection_audits_audience_check` — `CHECK (audience = 'private_campaign'::text)`
- `private_inspiration_selection_audits_campaign_revision_check` — `CHECK (campaign_revision > 0)`
- `private_inspiration_selection_audits_check` — `CHECK (cursor_after >= cursor_before)`
- `private_inspiration_selection_audits_check1` — `CHECK (selected_source_id IS NOT NULL AND selected_source_version IS NOT NULL AND selected_source_digest IS NOT NULL AND no_selection_reason IS NULL AND sample_numerator IS NOT NULL AND sample_denominator IS NOT NULL AND sample_denominator > 0 AND sample_numerator >= 0 AND sample_numerator < sample_denominator AND cursor_after > cursor_before OR selected_source_id IS NULL AND selected_source_version IS NULL AND selected_source_digest IS NULL AND no_selection_reason IS NOT NULL AND sample_numerator IS NULL AND sample_denominator IS NULL AND cursor_after = cursor_before)`
- `private_inspiration_selection_audits_created_at_epoch_check` — `CHECK (created_at_epoch >= 0)`
- `private_inspiration_selection_audits_cursor_before_check` — `CHECK (cursor_before >= 0)`
- `private_inspiration_selection_audits_eligible_set_digest_check` — `CHECK (eligible_set_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_selection_audits_idempotency_key_check` — `CHECK (octet_length(idempotency_key) >= 1 AND octet_length(idempotency_key) <= 128 AND idempotency_key ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_selection_audits_media_check` — `CHECK (media = ANY (ARRAY['text'::text, 'image'::text, 'recap'::text]))`
- `private_inspiration_selection_audits_no_selection_reason_check` — `CHECK (no_selection_reason IS NULL OR (no_selection_reason = ANY (ARRAY['deployment_disabled'::text, 'global_kill_switch'::text, 'campaign_disabled'::text, 'campaign_paused'::text, 'safety_incomplete'::text, 'no_eligible_sources'::text])))`
- `private_inspiration_selection_audits_request_fingerprint_check` — `CHECK (request_fingerprint ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_selection_audits_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_selection_audits_seed_reference_check` — `CHECK (octet_length(seed_reference) >= 1 AND octet_length(seed_reference) <= 128 AND seed_reference ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_selection_audits_selection_id_check` — `CHECK (octet_length(selection_id) >= 1 AND octet_length(selection_id) <= 128 AND selection_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_selection_audits_trigger_window_id_check` — `CHECK (octet_length(trigger_window_id) >= 1 AND octet_length(trigger_window_id) <= 128 AND trigger_window_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_selection_audits_turn_number_check` — `CHECK (turn_number >= 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_selection_campaign_session_id_idempoten_key` — `CREATE UNIQUE INDEX private_inspiration_selection_campaign_session_id_idempoten_key ON public.private_inspiration_selection_audits USING btree (campaign_session_id, idempotency_key)`
- `private_inspiration_selection_history_idx` — `CREATE INDEX private_inspiration_selection_history_idx ON public.private_inspiration_selection_audits USING btree (campaign_session_id, turn_number DESC, selection_id)`

</details>

### `private_inspiration_source_media`

**Purpose.** Media modes in which a private source version may be transformed/presented.

**Access pattern.** Registration inserts allowed media values. Candidate selection and consent checks join by source/version/media; presentation fails closed if the requested medium is absent.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:1435` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `media` | `text` | required; PK component; checked | Controlled output medium required by source and consent eligibility. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_source_media_pkey` — `PRIMARY KEY (source_id, source_version, media)`
- `private_inspiration_source_media_source_id_source_version_fkey` — `FOREIGN KEY (source_id, source_version) REFERENCES private_inspiration_sources(source_id, source_version) ON DELETE CASCADE`
- `private_inspiration_source_media_media_check` — `CHECK (media = ANY (ARRAY['text'::text, 'image'::text, 'recap'::text]))`

</details>

### `private_inspiration_source_participants`

**Purpose.** Many-to-many mapping from a source version to represented/affected participants.

**Access pattern.** Registration inserts the source’s participant set. Consent and exclusion checks join on the composite source key and participant ID. Rows are immutable for that source version.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:131` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `participant_id` | `text` | required; PK component; FK → private_inspiration_participants(participant_id) | Identifier for the associated participant; used to scope, join, or correlate this row. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_source_participants_pkey` — `PRIMARY KEY (source_id, source_version, participant_id)`
- `private_inspiration_source_partic_source_id_source_version_fkey` — `FOREIGN KEY (source_id, source_version) REFERENCES private_inspiration_sources(source_id, source_version) ON DELETE CASCADE`
- `private_inspiration_source_participants_participant_id_fkey` — `FOREIGN KEY (participant_id) REFERENCES private_inspiration_participants(participant_id)`

</details>

### `private_inspiration_source_sensitivities`

**Purpose.** Sensitivity labels attached to a private source version.

**Access pattern.** Registration inserts the label set; selection compares it with campaign allowances, lines/veils, and grant sensitivity scope. Rows are immutable for that source version.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:141` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `sensitivity_code` | `text` | required; PK component; checked | Controlled sensitivity code discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_source_sensitivities_pkey` — `PRIMARY KEY (source_id, source_version, sensitivity_code)`
- `private_inspiration_source_sensit_source_id_source_version_fkey` — `FOREIGN KEY (source_id, source_version) REFERENCES private_inspiration_sources(source_id, source_version) ON DELETE CASCADE`
- `private_inspiration_source_sensitivities_sensitivity_code_check` — `CHECK (octet_length(sensitivity_code) >= 1 AND octet_length(sensitivity_code) <= 128 AND sensitivity_code ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `private_inspiration_source_themes`

**Purpose.** Theme-pack scopes in which a private source version is eligible.

**Access pattern.** Registration inserts theme mappings. Selection joins the campaign’s pinned/current theme against `(source_id, source_version, theme_pack_id)` so material never leaks across theme scopes.

**Migration source(s).** `migrations/0018_private_inspiration_theme_scope.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:1447` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; FK → private_inspiration_sources(source_id, source_version) ON DELETE CASCADE | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `theme_pack_id` | `text` | required; PK component; checked | Theme-pack identifier that makes a source version eligible in that theme. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_source_themes_pkey` — `PRIMARY KEY (source_id, source_version, theme_pack_id)`
- `private_inspiration_source_themes_source_id_source_version_fkey` — `FOREIGN KEY (source_id, source_version) REFERENCES private_inspiration_sources(source_id, source_version) ON DELETE CASCADE`
- `private_inspiration_source_themes_theme_pack_id_check` — `CHECK (octet_length(theme_pack_id) >= 1 AND octet_length(theme_pack_id) <= 128 AND theme_pack_id ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

### `private_inspiration_source_usage`

**Purpose.** Append-only cooldown history recording when a selected source version was used in a campaign.

**Access pattern.** Selection inserts one row per successful selection. Future candidate scans use campaign/source and `next_eligible_turn` to enforce cooldown; rows cascade with the selection audit/campaign.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:1844` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `selection_id` | `text` | required; PK; FK → private_inspiration_selection_audits(selection_id) ON DELETE CASCADE | Identifier for the associated selection; used to scope, join, or correlate this row. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `source_id` | `text` | required; FK → private_inspiration_sources(source_id, source_version, source_digest) | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; FK → private_inspiration_sources(source_id, source_version, source_digest) | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `source_digest` | `text` | required; FK → private_inspiration_sources(source_id, source_version, source_digest) | Deterministic digest of source, used for integrity/equality checks without retaining the raw input. |
| `turn_number` | `bigint` | required; checked | Campaign/play-session turn ordinal used for ordering and cooldown/history queries. |
| `next_eligible_turn` | `bigint` | required; checked | First turn on which cooldown permits this source version to be selected again. |
| `created_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when created occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_source_usage_pkey` — `PRIMARY KEY (selection_id)`
- `private_inspiration_source_us_source_id_source_version_sou_fkey` — `FOREIGN KEY (source_id, source_version, source_digest) REFERENCES private_inspiration_sources(source_id, source_version, source_digest)`
- `private_inspiration_source_usage_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_source_usage_selection_id_fkey` — `FOREIGN KEY (selection_id) REFERENCES private_inspiration_selection_audits(selection_id) ON DELETE CASCADE`
- `private_inspiration_source_usage_check` — `CHECK (next_eligible_turn > turn_number)`
- `private_inspiration_source_usage_created_at_epoch_check` — `CHECK (created_at_epoch >= 0)`
- `private_inspiration_source_usage_turn_number_check` — `CHECK (turn_number >= 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_usage_lookup_idx` — `CREATE INDEX private_inspiration_usage_lookup_idx ON public.private_inspiration_source_usage USING btree (campaign_session_id, source_id, turn_number DESC)`

</details>

### `private_inspiration_sources`

**Purpose.** Reviewed, versioned registry entry for private inspiration source material. It stores minimized metadata/provenance, not the protected source body.

**Access pattern.** Offline registration inserts `(source_id, source_version)` plus child mappings; review updates screening state/evidence. Selection joins runtime projections, grants, vetoes, themes, media, sensitivity, and cooldown data. Composite indexes support current reviewed/enabled candidate scans.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:659` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `source_id` | `text` | required; PK component; composite unique; checked | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | required; PK component; composite unique; checked | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `source_digest` | `text` | required; composite unique; checked | Deterministic digest of source, used for integrity/equality checks without retaining the raw input. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `category_id` | `text` | required; checked | Controlled inspiration category used in eligibility and category-scoped veto checks. |
| `owner_participant_id` | `text` | required; FK → private_inspiration_participants(participant_id) | Identifier for the associated owner participant; used to scope, join, or correlate this row. |
| `review_state` | `text` | required; checked | Controlled source review lifecycle state. |
| `q11_screened` | `boolean` | required; default `false`; checked | Boolean flag indicating whether q11 screened is true for this row. |
| `audience` | `text` | required; checked | Controlled audience scope required by campaign policy and consent. |
| `transformation` | `text` | required; checked | Controlled transformation permission granted for private source material. |
| `provenance_digest` | `text` | required; checked | Deterministic digest of provenance, used for integrity/equality checks without retaining the raw input. |
| `review_evidence_digest` | `text` | nullable; checked | Deterministic digest of review evidence, used for integrity/equality checks without retaining the raw input. |
| `reviewer_id` | `text` | nullable; checked | Identifier for the associated reviewer; used to scope, join, or correlate this row. |
| `reviewed_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when reviewed occurred. |
| `expires_at_epoch` | `bigint` | nullable; checked | UTC Unix epoch seconds when expires occurred. |
| `registered_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when registered occurred. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_sources_pkey` — `PRIMARY KEY (source_id, source_version)`
- `private_inspiration_sources_source_id_source_digest_key` — `UNIQUE (source_id, source_digest)`
- `private_inspiration_sources_source_id_source_version_source_key` — `UNIQUE (source_id, source_version, source_digest)`
- `private_inspiration_sources_owner_participant_id_fkey` — `FOREIGN KEY (owner_participant_id) REFERENCES private_inspiration_participants(participant_id)`
- `private_inspiration_sources_audience_check` — `CHECK (audience = 'private_campaign'::text)`
- `private_inspiration_sources_category_id_check` — `CHECK (octet_length(category_id) >= 1 AND octet_length(category_id) <= 128 AND category_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_sources_check` — `CHECK (review_state = 'pending'::text AND NOT q11_screened AND review_evidence_digest IS NULL AND reviewer_id IS NULL AND reviewed_at_epoch IS NULL OR (review_state = ANY (ARRAY['approved'::text, 'rejected'::text, 'quarantined'::text])) AND review_evidence_digest IS NOT NULL AND reviewer_id IS NOT NULL AND reviewed_at_epoch IS NOT NULL)`
- `private_inspiration_sources_check1` — `CHECK (review_state <> 'approved'::text OR q11_screened)`
- `private_inspiration_sources_expires_at_epoch_check` — `CHECK (expires_at_epoch >= 0)`
- `private_inspiration_sources_provenance_digest_check` — `CHECK (provenance_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_sources_registered_at_epoch_check` — `CHECK (registered_at_epoch >= 0)`
- `private_inspiration_sources_review_evidence_digest_check` — `CHECK (review_evidence_digest IS NULL OR review_evidence_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_sources_review_state_check` — `CHECK (review_state = ANY (ARRAY['pending'::text, 'approved'::text, 'rejected'::text, 'quarantined'::text]))`
- `private_inspiration_sources_reviewed_at_epoch_check` — `CHECK (reviewed_at_epoch >= 0)`
- `private_inspiration_sources_reviewer_id_check` — `CHECK (reviewer_id IS NULL OR reviewer_id ~ '^operator:[0-9a-f]{32}$'::text)`
- `private_inspiration_sources_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_sources_source_digest_check` — `CHECK (source_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `private_inspiration_sources_source_id_check` — `CHECK (source_id ~ '^event-source-[0-9a-f]{24}$'::text)`
- `private_inspiration_sources_source_version_check` — `CHECK (source_version > 0)`
- `private_inspiration_sources_transformation_check` — `CHECK (transformation = 'high_fiction_distance_v1'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_sources_source_id_source_digest_key` — `CREATE UNIQUE INDEX private_inspiration_sources_source_id_source_digest_key ON public.private_inspiration_sources USING btree (source_id, source_digest)`
- `private_inspiration_sources_source_id_source_version_source_key` — `CREATE UNIQUE INDEX private_inspiration_sources_source_id_source_version_source_key ON public.private_inspiration_sources USING btree (source_id, source_version, source_digest)`

</details>

### `private_inspiration_vetoes`

**Purpose.** Participant or operator intervention that blocks a category or exact source within a campaign.

**Access pattern.** Control paths insert scoped veto records; selection probes active vetoes by campaign/participant/category/source. Rows preserve actor kind and evidence code. The nullable participant supports operator/global interventions introduced by later migrations.

**Migration source(s).** `migrations/0013_private_inspiration_consent.sql`, `migrations/0016_private_inspiration_interventions.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/inspiration.rs:2293` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `veto_id` | `text` | required; PK; checked | Identifier for the associated veto; used to scope, join, or correlate this row. |
| `schema_version` | `bigint` | required; checked | Version of the row or serialized contract required to interpret this record safely. |
| `campaign_session_id` | `text` | required; FK → campaign_sessions(id) ON DELETE CASCADE | Campaign that owns/scopes the row and is the principal partition key for access. |
| `participant_id` | `text` | nullable; FK → private_inspiration_participants(participant_id); checked | Identifier for the associated participant; used to scope, join, or correlate this row. |
| `scope_kind` | `text` | required; checked | Controlled scope kind discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `category_id` | `text` | nullable; checked | Identifier for the associated category; used to scope, join, or correlate this row. |
| `source_id` | `text` | nullable; FK → private_inspiration_sources(source_id, source_version, source_digest); checked | Identifier for the associated source; used to scope, join, or correlate this row. |
| `source_version` | `bigint` | nullable; FK → private_inspiration_sources(source_id, source_version, source_digest); checked | Immutable version number of the private source; always paired with `source_id` and often `source_digest`. |
| `source_digest` | `text` | nullable; FK → private_inspiration_sources(source_id, source_version, source_digest); checked | Deterministic digest of source, used for integrity/equality checks without retaining the raw input. |
| `state` | `text` | required; default `'active'`; checked | Lifecycle state; allowed values and cross-field invariants are enforced by CHECK constraints below. |
| `veto_code` | `text` | required; checked | Controlled veto code discriminator; accepted values are enforced by CHECK constraints where applicable. |
| `created_at_epoch` | `bigint` | required; checked | UTC Unix epoch seconds when created occurred. |
| `actor_kind` | `text` | required; default `'participant'`; checked | Controlled actor kind discriminator; accepted values are enforced by CHECK constraints where applicable. |

<details>
<summary>Exact table constraints</summary>

- `private_inspiration_vetoes_pkey` — `PRIMARY KEY (veto_id)`
- `private_inspiration_vetoes_campaign_session_id_fkey` — `FOREIGN KEY (campaign_session_id) REFERENCES campaign_sessions(id) ON DELETE CASCADE`
- `private_inspiration_vetoes_participant_id_fkey` — `FOREIGN KEY (participant_id) REFERENCES private_inspiration_participants(participant_id)`
- `private_inspiration_vetoes_source_id_source_version_source_fkey` — `FOREIGN KEY (source_id, source_version, source_digest) REFERENCES private_inspiration_sources(source_id, source_version, source_digest)`
- `private_inspiration_veto_actor_check` — `CHECK (actor_kind = 'participant'::text AND participant_id IS NOT NULL OR actor_kind = 'campaign_owner'::text AND participant_id IS NULL)`
- `private_inspiration_vetoes_actor_kind_check` — `CHECK (actor_kind = ANY (ARRAY['participant'::text, 'campaign_owner'::text]))`
- `private_inspiration_vetoes_category_id_check` — `CHECK (category_id IS NULL OR octet_length(category_id) >= 1 AND octet_length(category_id) <= 128 AND category_id ~ '^[A-Za-z0-9_.:-]+$'::text)`
- `private_inspiration_vetoes_check` — `CHECK (scope_kind = 'campaign'::text AND category_id IS NULL AND source_id IS NULL AND source_version IS NULL AND source_digest IS NULL OR scope_kind = 'category'::text AND category_id IS NOT NULL AND source_id IS NULL AND source_version IS NULL AND source_digest IS NULL OR scope_kind = 'source_version'::text AND category_id IS NULL AND source_id IS NOT NULL AND source_version IS NOT NULL AND source_digest IS NOT NULL)`
- `private_inspiration_vetoes_created_at_epoch_check` — `CHECK (created_at_epoch >= 0)`
- `private_inspiration_vetoes_schema_version_check` — `CHECK (schema_version = 1)`
- `private_inspiration_vetoes_scope_kind_check` — `CHECK (scope_kind = ANY (ARRAY['campaign'::text, 'category'::text, 'source_version'::text]))`
- `private_inspiration_vetoes_state_check` — `CHECK (state = 'active'::text)`
- `private_inspiration_vetoes_veto_code_check` — `CHECK (veto_code = ANY (ARRAY['participant_veto'::text, 'safety_veto'::text, 'privacy_veto'::text]))`
- `private_inspiration_vetoes_veto_id_check` — `CHECK (octet_length(veto_id) >= 1 AND octet_length(veto_id) <= 128 AND veto_id ~ '^[A-Za-z0-9_.:-]+$'::text)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `private_inspiration_veto_lookup_idx` — `CREATE INDEX private_inspiration_veto_lookup_idx ON public.private_inspiration_vetoes USING btree (campaign_session_id, scope_kind, category_id, source_id)`

</details>


## Custom action points

### `custom_action_point_balances`

**Purpose.** Materialized current custom-action-point balance for one account’s runtime character in a campaign/play-session context.

**Access pattern.** **Repository primitive, not yet wired into the authoritative turn/application flow.** Helper methods read by the composite primary key, upsert grants/refunds, and lock spends `FOR UPDATE`; each helper call pairs its balance change with a ledger insert in its own transaction. The balance table has no FKs, so account/campaign/character/play-session consistency is application-enforced. On grant/refund conflict, the upsert increments `balance` and updates only `updated_at`; it does not replace `play_session_id`, so the stored session can remain the first session associated with that account/campaign/runtime-character key.

**Migration source(s).** `migrations/0031_custom_action_points.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/action_points.rs:105` (INSERT/SELECT/UPDATE)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `account_id` | `text` | required; PK component | Account participating in or owning the scoped relation. |
| `campaign_id` | `text` | required; PK component | Campaign identifier used by the custom-action-point subsystem. |
| `runtime_character_id` | `text` | required; PK component | Identifier for the associated runtime character; used to scope, join, or correlate this row. |
| `play_session_id` | `text` | required | Play-session/lobby that scopes the row. |
| `balance` | `integer` | required; default `0`; checked | Current materialized non-negative point balance checked/updated under row lock. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `custom_action_point_balances_pkey` — `PRIMARY KEY (account_id, campaign_id, runtime_character_id)`
- `custom_action_point_balances_balance_check` — `CHECK (balance >= 0)`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `idx_cap_balances_account_campaign` — `CREATE INDEX idx_cap_balances_account_campaign ON public.custom_action_point_balances USING btree (account_id, campaign_id)`

</details>

### `custom_action_point_ledger`

**Purpose.** Append-only custom-action-point grant/spend/refund ledger.

**Access pattern.** **Repository primitive, not yet wired into the authoritative turn/application flow.** A helper first inserts with `ON CONFLICT (idempotency_key, reason) DO NOTHING`; conflict returns the current balance without reapplying the delta. This is not verified “exact replay”: there is no request fingerprint or comparison of account, campaign, runtime character, play session, turn revision, or amount, and uniqueness is global across all entities. Session history reads by `play_session_id`. Account/campaign/runtime-hero FKs use default `NO ACTION`, so any ledger row blocks deletion of those referenced parents; `play_session_id` has no FK and `turn_revision`/`amount` have no range CHECKs.

**Migration source(s).** `migrations/0031_custom_action_points.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/action_points.rs:141` (INSERT/SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `id` | `text` | required; PK | Stable application-generated identifier for the `custom_action_point_ledger` row. |
| `account_id` | `text` | required; FK → accounts(id) | Account participating in or owning the scoped relation. |
| `campaign_id` | `text` | required; FK → campaign_sessions(id) | Campaign identifier used by the custom-action-point subsystem. |
| `runtime_character_id` | `text` | required; FK → hero_characters(id) | Identifier for the associated runtime character; used to scope, join, or correlate this row. |
| `play_session_id` | `text` | required | Play-session/lobby that scopes the row. |
| `turn_revision` | `bigint` | required | Turn-control revision associated with this point-ledger entry. |
| `amount` | `integer` | required | Point magnitude interpreted by `reason`; repository callers expect a positive value, but migration 0031 does not add a database range CHECK. |
| `reason` | `text` | required; composite unique; checked | Controlled reason code that determines the semantic effect of the row. |
| `idempotency_key` | `text` | required; composite unique | Opaque client/operator retry key within the table’s documented scope. |
| `created_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp when the row was inserted. |

<details>
<summary>Exact table constraints</summary>

- `custom_action_point_ledger_pkey` — `PRIMARY KEY (id)`
- `custom_action_point_ledger_idempotency_key_reason_key` — `UNIQUE (idempotency_key, reason)`
- `custom_action_point_ledger_account_id_fkey` — `FOREIGN KEY (account_id) REFERENCES accounts(id)`
- `custom_action_point_ledger_campaign_id_fkey` — `FOREIGN KEY (campaign_id) REFERENCES campaign_sessions(id)`
- `custom_action_point_ledger_runtime_character_id_fkey` — `FOREIGN KEY (runtime_character_id) REFERENCES hero_characters(id)`
- `custom_action_point_ledger_reason_check` — `CHECK (reason = ANY (ARRAY['initial_grant'::text, 'earned'::text, 'custom_action_spent'::text, 'administrative_refund'::text]))`

</details>

<details>
<summary>Supporting and unique indexes</summary>

- `custom_action_point_ledger_idempotency_key_reason_key` — `CREATE UNIQUE INDEX custom_action_point_ledger_idempotency_key_reason_key ON public.custom_action_point_ledger USING btree (idempotency_key, reason)`
- `idx_cap_ledger_session` — `CREATE INDEX idx_cap_ledger_session ON public.custom_action_point_ledger USING btree (play_session_id, created_at DESC)`

</details>


## Operations and recovery

### `operator_recovery_status`

**Purpose.** Singleton operator-maintained summary of the latest successful backup and restore test.

**Access pattern.** Migration seeds exactly one row. Server operations snapshots read it; backup/restore tooling is expected to update it after verified operations. Nullable fields mean “not yet recorded,” and the singleton CHECK prevents multiple status rows.

**Migration source(s).** `migrations/0024_operator_recovery_status.sql`

**SQL references (runtime, maintenance, and tests).** `crates/game-server/src/repository/operations.rs:523` (SELECT)

| Field | PostgreSQL type | Null/default/key rules | Usage |
|---|---|---|---|
| `singleton` | `boolean` | required; default `true`; PK; checked | Boolean flag indicating whether singleton is true for this row. |
| `schema_version` | `smallint` | required; default `1`; checked | Version of the row or serialized contract required to interpret this record safely. |
| `last_backup_completed_at` | `timestamp with time zone` | nullable; checked | Timestamp when last backup completed occurred or becomes effective. |
| `last_backup_vault_digest` | `text` | nullable; checked | Deterministic digest of last backup vault, used for integrity/equality checks without retaining the raw input. |
| `last_restore_test_completed_at` | `timestamp with time zone` | nullable; checked | Timestamp when last restore test completed occurred or becomes effective. |
| `last_restore_test_result` | `text` | nullable; checked | Controlled result of the latest operator restore test; null until a test has been recorded. |
| `last_restore_source_digest` | `text` | nullable; checked | Deterministic digest of last restore source, used for integrity/equality checks without retaining the raw input. |
| `updated_at` | `timestamp with time zone` | required; default `CURRENT_TIMESTAMP` | Database timestamp of the latest mutable-row update. |

<details>
<summary>Exact table constraints</summary>

- `operator_recovery_status_pkey` — `PRIMARY KEY (singleton)`
- `operator_recovery_status_check` — `CHECK ((last_backup_completed_at IS NULL) = (last_backup_vault_digest IS NULL))`
- `operator_recovery_status_check1` — `CHECK ((last_restore_test_completed_at IS NULL) = (last_restore_test_result IS NULL))`
- `operator_recovery_status_check2` — `CHECK (last_restore_source_digest IS NULL OR last_restore_test_completed_at IS NOT NULL)`
- `operator_recovery_status_last_backup_vault_digest_check` — `CHECK (last_backup_vault_digest IS NULL OR last_backup_vault_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `operator_recovery_status_last_restore_source_digest_check` — `CHECK (last_restore_source_digest IS NULL OR last_restore_source_digest ~ '^sha256:[0-9a-f]{64}$'::text)`
- `operator_recovery_status_last_restore_test_result_check` — `CHECK (last_restore_test_result IS NULL OR (last_restore_test_result = ANY (ARRAY['passed'::text, 'failed'::text])))`
- `operator_recovery_status_schema_version_check` — `CHECK (schema_version = 1)`
- `operator_recovery_status_singleton_check` — `CHECK (singleton)`

</details>

## Schema maintenance notes

1. Treat migrations as the schema authority; update this document whenever a later migration changes a table, constraint, or index.
2. Keep account authorization predicates in SQL (`owner_account_id`, `account_id`, active membership), not only in route code.
3. Keep idempotency fingerprint checks and state mutation in one transaction. A unique key alone prevents duplicates but does not prove that a replay matches the original request.
4. Preserve the split between reusable `player_characters` and campaign runtime state in `hero_characters`/`campaign_character_instances`.
5. Do not treat the migration-0030 lobby/turn-control tables as active until repository methods perform the intended locked/revisioned writes.
6. For external artifacts, PostgreSQL is metadata/provenance authority; file/object bytes remain in protected storage and must be deleted before metadata cleanup commits.
