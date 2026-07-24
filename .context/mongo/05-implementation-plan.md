# MongoDB Greenfield Rewrite Implementation Plan

> **Execution contract:** implement one vertical slice at a time, test it against a real replica-set MongoDB, and do not remove PostgreSQL until all replacement gates pass. There is no legacy data migration or dual write.

**Goal:** replace the current SQLx/PostgreSQL persistence layer with a strict, secure MongoDB design that supports gated accounts, account-owned character libraries, campaign snapshots/runtime, enemies/events, battles, AI GM turns, generated assets, BDE, privacy, and recovery.

**Architecture:** `game-core` remains deterministic. `game-server` owns authorization/orchestration and uses a neutral `Repository` facade backed by the official async MongoDB Rust driver. Mutable aggregates use optimistic revisions; cross-aggregate commands use short transactions; unbounded histories use append-only collections. An optional DragonflyDB (Redis-compatible) layer provides session caching, rate limiting, and real-time pub/sub — see [07-dragonflydb-integration.md](07-dragonflydb-integration.md).

**Technology baseline:** Rust/Tokio; the official [MongoDB Rust Driver](https://www.mongodb.com/docs/drivers/rust/current/) via the `mongodb` crate (`3.8.0` at planning time, requiring Rust 1.88); use the driver's re-exported `mongodb::bson` types to avoid BSON-major mismatches; MongoDB replica set; rustls/TLS; Argon2id; XChaCha20-Poly1305 or vetted Mongo in-use encryption; external protected media storage. The optional DragonflyDB layer uses the officially supported Rust SDK [`redis-rs`](https://github.com/redis-rs/redis-rs), published as `redis = "1.4.1"`, plus `deadpool-redis = "0.23.0"` for pooling.

Treat the [current MongoDB Rust Driver documentation](https://www.mongodb.com/docs/drivers/rust/current/) as the authoritative SDK reference for client construction, connection options, TLS, authentication, CRUD, indexes, sessions, transactions, change streams, monitoring, and driver upgrade guidance. Pin the selected crate version in `Cargo.lock`; do not assume the documentation's `current` version matches the lockfile without checking during implementation.

## Phase 0 — Resolve product/security decisions and freeze behavior

### Task 0.1: Record mandatory decisions

**Files:**
- Update: `.context/mongo/06-requirements-and-decisions.md`

Decide and mark resolved:

- ruleset strategy (keep current mechanics for persistence rewrite, then SRD v5.2.1 is recommended);
- account-session idle/absolute lifetime and 24-hour sign-up semantics;
- email KMS/secret source and rotation ownership;
- campaign party maximum/start policy;
- enemy permission interpretation;
- XP versus milestone default;
- BDE starting/max/cost/earning/refund policy;
- custom prompt/narration retention and encryption;
- object storage/image provider/moderation;
- deletion/export retention;
- production MongoDB deployment/backup provider.

**Gate:** no implementation may improvise an unresolved security/economy rule.

### Task 0.2: Preserve useful behavior tests

**Files:**
- Read/update tests under `crates/game-core/src/`, `crates/game-server/src/`, `crates/game-server/tests/`, and `crates/web/tests/`.
- Add: `crates/game-server/tests/mongo_contract/behavior_baseline.rs`

Freeze tests for:

- two-account isolation;
- level-less player library/campaign instance separation;
- snapshots and exact idempotency;
- active campaign deletion rejection;
- encounter turns/rewards/restarts;
- BDE no-double-spend;
- generation lease/publication;
- private-inspiration consent/veto/safety;
- protected asset access.

Run before changes:

```bash
cargo test --workspace
```

If current failures exist, record them separately; do not relabel regressions as baseline.

**Commit:** `test: freeze persistence rewrite behavior contracts`

## Phase 1 — MongoDB infrastructure and schema control

### Task 1.1: Add driver and neutral configuration

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/game-server/Cargo.toml`
- Modify: `crates/game-server/src/config.rs`
- Modify: `crates/game-server/src/error.rs`
- Add: `crates/game-server/src/persistence/mod.rs`
- Add: `crates/game-server/src/persistence/mongo.rs`
- Add: `crates/game-server/src/cache/mod.rs` (DragonflyDB CacheService skeleton, disabled by default)

Changes:

- add the official [MongoDB Rust Driver](https://www.mongodb.com/docs/drivers/rust/current/) as `mongodb = "3.8.0"`; use its re-exported `mongodb::bson` types rather than an incompatible independent BSON major;
- add DragonflyDB's officially supported Rust SDK [`redis-rs`](https://github.com/redis-rs/redis-rs) as `redis = "1.4.1"`, plus `deadpool-redis = "0.23.0"` for the optional DragonflyDB cache/pub-sub layer (see [07-dragonflydb-integration.md](07-dragonflydb-integration.md));
- replace `DATABASE_URL` with required `MONGODB_URI` and `MONGODB_DATABASE`;
- add optional `DRAGONFLY_URL`, `DRAGONFLY_POOL_SIZE`, `DRAGONFLY_TIMEOUT_MS`, `DRAGONFLY_ENABLED`;
- add bounded pool, connect/server-selection/socket/transaction timeouts;
- validate database-name allowlist and hosted TLS requirements;
- expose a redacted typed `PersistenceConfig` and `DragonflyConfig`;
- map duplicate key, validation, timeout, transient transaction, unknown commit result, not-found, and revision conflict precisely; no broad catch-all suppression.

Test malformed/missing URI, unsafe hosted URI, redaction, database-name validation, timeout bounds, and DragonflyDB connection failure with `DRAGONFLY_ENABLED=true` (must not crash, must fall through).

### Task 1.2: Run a single-node replica set locally

**Files:**
- Modify: `compose.yaml`
- Delete at final cutover: `scripts/postgres-roles.sql`
- Delete at final cutover: `scripts/check-postgres-role-policy.sh`
- Add: `scripts/mongo-rs-init.js`
- Add: `scripts/check-mongo-role-policy.sh`
- Add/update: local environment example/documentation

Requirements:

- pin a supported MongoDB 8.x production release/image by digest; no floating `latest`;
- use authentication and a single-node `rs0` replica set;
- bind local ports to loopback;
- health check waits for primary and authenticated ping;
- separate least-privilege application and schema-admin users;
- persistent volume; no secret committed in Compose;
- CI/test helper can create a unique disposable database.

Verify:

```bash
docker compose up -d mongodb
mongosh "$MONGODB_URI" --quiet --eval 'db.runCommand({ping:1})'
mongosh "$MONGODB_URI" --quiet --eval 'rs.status().myState'
```

The replica-set state must be primary (`1`) before transaction tests.

### Task 1.3: Build idempotent schema/index reconciler

**Files:**
- Add: `crates/game-server/src/persistence/schema.rs`
- Add: `crates/game-server/src/persistence/indexes.rs`
- Add: `crates/game-server/src/persistence/validators.rs`
- Replace: `crates/game-server/src/bin/database-migrate.rs` with `crates/game-server/src/bin/mongo-admin.rs`
- Update/remove obsolete `[[bin]]` entries in `crates/game-server/Cargo.toml`

Commands:

```text
mongo-admin schema apply
mongo-admin schema verify
mongo-admin indexes verify
```

Implement exact named validators/indexes from `01-target-data-model.md`. Detect option/key conflicts and extra obsolete managed indexes. Store schema bundle version/digest in `system_settings`.

Hosted app startup is verify-only and fails closed. Schema apply uses a separate privileged identity.

Tests:

- clean database apply then verify;
- second apply is no-op;
- altered index/validator is detected;
- malformed representative documents fail;
- maximum-valid documents pass;
- TTL index is a single date field with `expireAfterSeconds:0`;
- partial unique indexes enforce one open session/encounter/current template.

### Task 1.4: Add replica-set integration-test harness

**Files:**
- Add: `crates/game-server/tests/support/mongo.rs`
- Add: `crates/game-server/tests/mongo_contract/schema.rs`
- Add: `crates/game-server/tests/mongo_contract/transactions.rs`

Harness rules:

- require explicit `MONGODB_TEST_URI`, never production URI;
- create random test database per test binary/suite;
- apply schema bundle;
- drop database in cleanup with a run ID safeguard;
- serialize only schema-level global operations;
- permit parallel tests across unique DBs;
- test transient transaction retry/unknown commit behavior without external side effects.

**Gate:** transaction test must fail on standalone MongoDB and pass on replica set.

**Commit:** `feat: add replica-set mongo persistence foundation`

## Phase 2 — Repository facade, models, transactions, receipts, audits

### Task 2.1: Replace concrete PostgreSQL identity

**Files:**
- Rewrite: `crates/game-server/src/repository.rs`
- Modify: `crates/game-server/src/context.rs`
- Modify call sites in `application.rs`, `application/*.rs`, `auth.rs`, `scene_images.rs`, `generation_ledger.rs`, `inspiration.rs`

Introduce cloneable neutral `Repository` containing typed Mongo collections/services. Remove `PostgresRepository` from application signatures. Do not create a temporary fake SQL-shaped abstraction.

Preserve domain-level method names where behavior remains correct, but redesign methods that expose rows, SQL errors, `PgPool`, or PostgreSQL transactions.

### Task 2.2: Create durable model modules

**Files:**
- Add directory: `crates/game-server/src/persistence/models/`
- Add modules: `account.rs`, `campaign.rs`, `character.rs`, `content.rs`, `play.rs`, `encounter.rs`, `event.rs`, `generation.rs`, `receipt.rs`, `audit.rs`, `privacy.rs`, `operations.rs`

Each model:

- derives Mongo/BSON serde with `deny_unknown_fields` where supported;
- has typed ID/digest/date/state fields;
- validates schema version/revision/ranges/array limits;
- separates storage DTO from public API DTO and secrets;
- exposes deterministic canonical fingerprint material that excludes timestamps/random DB fields;
- has round-trip and maximum BSON-size tests.

Do not serialize domain errors, raw email/tokens, crypto keys, hidden enemy fields, or private source bodies in debug output.

### Task 2.3: Implement transaction and optimistic-write helpers

**Files:**
- Add: `crates/game-server/src/persistence/transaction.rs`
- Add: `crates/game-server/src/persistence/revision.rs`

Helpers must:

- use snapshot read concern + majority write concern;
- bound attempts/time;
- retry only MongoDB-labeled transient/unknown-commit cases;
- keep callbacks idempotent;
- require ownership/partition/state/revision in conditional filters;
- distinguish duplicate idempotency, revision conflict, and safe not-found;
- never retry validation/auth/authorization/domain errors.

### Task 2.4: Implement generic receipt and audit stores

**Files:**
- Replace SQL in: `crates/game-server/src/repository/operations.rs`
- Add/refactor: `repository/receipts.rs`, `repository/audits.rs`
- Update all domain modules that wrote specialized receipt/audit tables.

Create one exact-replay path keyed by scope and idempotency key. On collision compare actor, command kind, canonical fingerprint, target IDs, and expected revisions. Write receipt in the same transaction as mutation.

Audit payloads are category-specific, bounded, minimized, and exclude narration/private text/credentials.

**Gate:** concurrency test with two same-key commands commits once; changed-payload reuse conflicts.

**Commit:** `refactor: introduce mongo repository and exact command boundary`

## Phase 3 — Gated accounts, sessions, roles, and crypto

### Task 3.1: Implement email crypto/key management

**Files:**
- Add: `crates/game-server/src/crypto/email.rs`
- Add: `crates/game-server/src/crypto/keys.rs`
- Modify: `crates/game-server/src/config.rs`
- Add tests: `crates/game-server/tests/mongo_contract/email_crypto.rs`

Implement canonical email policy, keyed HMAC lookup, randomized AEAD with account/schema/key AAD, key IDs, constant-time comparisons, redaction, and rotation hooks. Keys must come from a secret/KMS provider, not MongoDB.

Tests include random ciphertext for same email, stable HMAC, wrong key/AAD rejection, duplicate normalized email, non-logging/debug redaction, and key rotation.

### Task 3.2: Implement admission token and 24-hour signup session

**Files:**
- Rewrite: `crates/game-server/src/repository/auth.rs`
- Rewrite relevant: `crates/game-server/src/auth.rs`
- Extend: `crates/game-server/src/bin/mongo-admin.rs`
- Add account/signup endpoints and SSR forms in existing web route/component paths.

Implement reserve/consume/revoke states and one active signup session per token. Store only digests. Enforce 24-hour expiry in query predicates and use TTL only for cleanup. Add CSRF and host-only secure cookie policy.

### Task 3.3: Implement credential signup/login/logout

Use Argon2id PHC with benchmarked parameters and bounded blocking pool/concurrency. Login by normalized username or email HMAC, generic failure, dummy verify on miss, throttle buckets, session issuance/rotation/revocation, and password/role session version.

Admin creation is CLI-only; web can create only user. Add CLI token revoke/list and session revoke/account disable commands with safe output.

### Task 3.4: Prove global authentication protection and isolation

Test matrix:

- all non-auth routes reject anonymous users;
- raw tokens never stored/logged;
- access token cannot create two sessions/accounts;
- expired/revoked/consumed token and expired signup session rejected before TTL;
- duplicate username/email rejected safely;
- user cannot self-promote;
- admin CLI creates admin and is audited;
- session fixation/CSRF/host mismatch/logout/password change behavior;
- account A cannot retrieve/mutate account B data by ID.

### Task 3.5: Wire DragonflyDB session cache and throttle (optional, after MongoDB auth works)

**Files:**
- Add: `crates/game-server/src/cache/session.rs`
- Add: `crates/game-server/src/cache/throttle.rs`
- Modify: `crates/game-server/src/context.rs` (inject `CacheService` into `ServerContext`)
- Modify: `crates/game-server/src/auth.rs` (session middleware reads cache first, falls through to MongoDB on miss)
- Add: `crates/game-server/tests/mongo_contract/dragonfly_cache.rs`

Implement read-through session cache, `DEL` on logout/revocation, `password_role_version` invalidation, and `INCR`/`EXPIRE` throttle buckets. Test: cache miss falls through, revocation deletes cache, `FLUSHDB` simulation, concurrent session creation (no stampede), and graceful degradation when DragonflyDB is down. See [07-dragonflydb-integration.md](07-dragonflydb-integration.md).

**Commit:** `feat: add access-token gated accounts and encrypted email`

## Phase 4 — Player-character library and campaign instances

### Task 4.1: Rewrite player-character repositories

**Files:**
- Rewrite: `repository/player_characters.rs`
- Remove legacy SQL/import handling: `repository/legacy.rs`, `bin/legacy-import.rs`
- Update: `application/player_characters.rs`
- Update web APIs/components/tests for authenticated owner IDs.

Implement draft/create/read/list/update/delete with account partition and revisions. No level/runtime fields in library documents. Delete rejects when any active campaign instance references the source character.

### Task 4.2: Merge hero runtime into campaign instance

**Files:**
- Rewrite: `repository/hero.rs`
- Update: `application/hero.rs`
- Rewrite affected game-core/server bridge DTOs if they currently require duplicate hero/campaign-instance IDs.

Create campaign instance from immutable source snapshot. Store progression/derived sheet/runtime under one aggregate. Preserve domain derivation and encounter synchronization; never let clients write derived maxima/features.

### Task 4.3: Add character image generation publication

Use `generation_jobs`/`generated_assets`; object bytes external. Publication transaction requires same character revision/owner and updates portrait pointer once. Asset retrieval resolves owner before serving.

**Gate:** same source character can produce independent instances in two campaigns; source update does not alter either; active reference blocks library deletion.

**Commit:** `feat: persist level-less characters and campaign runtime snapshots`

## Phase 5 — Campaign ownership, membership, lifecycle, and content pins

### Task 5.1: Rewrite campaign/membership/pin repositories

**Files:**
- Rewrite: `repository/memberships.rs`, `repository/pins.rs`, `repository/lifecycle.rs`, `application/lifecycle.rs`
- Update: `campaign_pins.rs`, `application.rs`

Implement owner creation, bounded embedded membership with revision CAS, invitations as separate TTL collection, member listing, archive, immutable rules snapshot sealing, and admin override audit.

### Task 5.2: Rewrite lobby/play-session state

Implement `play_sessions` with bounded participants/membership snapshot/start policy/mode/turn state. Use partial unique index for one waiting/active session per campaign. Make human/AI substitution and handoff explicit.

After the MongoDB transaction commits a play-session state change, publish a `CampaignEvent` notification to the DragonflyDB pub/sub channel `campaign:{campaign_id}:events` so connected clients receive real-time lobby updates. If DragonflyDB is disabled or unreachable, clients fall back to polling. See [07-dragonflydb-integration.md](07-dragonflydb-integration.md) §3.

### Task 5.3: Implement guarded campaign deletion

Rewrite `repository/lifecycle.rs` using the explicit collection manifest. Reject while waiting/active play session or active encounter exists. Preserve preparation/export/tombstone/exact replay and external-asset cleanup ordering. Use durable cleanup state machine if delete volume exceeds one transaction.

**Gate:** owner can manage own campaign, non-owner cannot discover/manage it, admin override is audited, active deletion fails, closed deletion leaves required tombstone/receipt and no accessible child/asset.

**Commit:** `feat: persist owned campaigns lobbies and guarded lifecycle`

## Phase 6 — Enemy/event templates and campaign instances

### Task 6.1: Add versioned enemy templates

**Files:**
- Add: `repository/enemies.rs`
- Add application/admin endpoints and DTOs under `application/` and web server functions/components.
- Extend `game-core` content/stat-block types without DB-specific types.

Admin-only create/update/archive; update creates a new immutable revision and marks it current. Validate complete stat block and ruleset compatibility. Campaign owner may instantiate/generate for owned campaign but not edit global templates.

### Task 6.2: Add campaign enemy snapshots and generation

Create snapshot/runtime aggregate with hidden/public projections. Typed generation produces a draft, deterministic validation approves it, and publication transaction binds prompt/config/content/source revisions.

### Task 6.3: Add versioned event templates and campaign events

**Files:**
- Add: `repository/events.rs`
- Refactor: `events.rs` to support Mongo template registry while preserving reviewed/minimized private runtime prompts.

Admin-only template CRUD/archive. Deterministic selection writes eligible-set digest/RNG facts/cooldown, snapshots template, and produces a campaign event state machine.

**Gate:** old template revisions remain readable, active instances unchanged, hidden enemy fields cannot reach player DTOs, non-admin mutation denied, event cooldown/safety deterministic.

**Commit:** `feat: add versioned enemies and campaign events`

## Phase 7 — Encounters, turns, random facts, and BDE

### Task 7.1: Generalize encounter persistence

**Files:**
- Add/rewrite: `repository/encounters.rs`
- Update: `application.rs` and relevant encounter service paths.
- Extend `game-core/src/encounter.rs` only through rules-engine tasks, not storage shortcuts.

Persist bounded multi-combatant state, initiative, round/actor, action economy, effects, objectives, reward state, and snapshots. Use one active encounter partial unique index per play session.

### Task 7.2: Implement turn-event commit

Load/compute outside transaction, then CAS play session/encounter/affected character/enemy revisions. Insert deterministic roll/effect event, minimized audit, and receipt atomically. Never invoke LLM in transaction.

### Task 7.3: Implement encounter completion/rewards

In one transaction synchronize campaign character runtime, enemy state, progression/reward effects, play mode, terminal encounter state, turn event, and receipt. Reward claim exactly once.

### Task 7.4: Rewrite BDE repository

**Files:**
- Rewrite: `repository/action_points.rs`

Materialize balance inside campaign character; append ledger entry for every delta. Custom action interpretation happens before transaction; commit verifies applicability/current legal set/revisions and balance, mutates mechanics and balance, inserts ledger/event/audit/receipt. Failure/rejection/timeout costs zero.

**Gate:** parallel turn/BDE/reward tests prove no double action, double reward, negative balance, duplicate roll, or cross-character/global idempotency collision.

**Commit:** `feat: commit mongo encounters turns rewards and bde atomically`

## Phase 8 — Typed AI GM, generation queue, presentations, and assets

### Task 8.1: Rewrite generation job/governance repositories

**Files:**
- Rewrite: `repository/jobs.rs`, `repository/governance.rs`, `generation_ledger.rs`
- Preserve/refactor: `generation.rs`, `typed_gm.rs`

Implement atomic claim/lease/heartbeat/settlement, bounded attempts, stale lease recovery, budget reservations, fingerprints, and no secrets/raw private prompts. External provider calls remain outside transactions.

### Task 8.2: Rewrite presentation/recap repositories

**Files:**
- Rewrite: `repository/presentations.rs`, `repository/recaps.rs`

Mechanics and presentation are separate. Version/regenerate prose without changing turn state; partial unique selected version per origin. Apply audience/private retention and protected-body policy.

### Task 8.3: Rewrite scene/image assets

**Files:**
- Rewrite: `repository/images.rs`, `scene_images.rs`

Use external temporary object key, validation/moderation/quarantine, digest/media checks, then publication transaction and authorized parent pointer. Delete rejected/orphaned bytes first.

### Task 8.4: Connect GM turn state machine

GM input contains minimized facts and legal action IDs. Typed proposal must bind play-session revision, event sequence, legal-action-set digest, prompt/policy/config fingerprints. Stale/malformed/unsafe proposals use authored fallback or reject; they never mutate mechanics directly.

**Gate:** worker crash/reclaim, provider timeout/rate-limit/malformed/unsafe response, stale proposal, duplicate publish, protected asset isolation, and restart tests.

**Commit:** `feat: persist typed gm generation and protected artifacts`

## Phase 9 — Private inspiration and safety

### Task 9.1: Rewrite source/participant/consent repositories

**Files:**
- Rewrite: `repository/inspiration.rs`, `inspiration.rs`
- Preserve external vault boundaries: `source_vault.rs`, `recovery_vault.rs` and their CLIs as adapted.

Use versioned sources with bounded embedded metadata/runtime projection; raw bodies/media stay protected externally. Implement participant verification, consent grants, revocation, campaign policy, vetoes, kill switch.

### Task 9.2: Rewrite deterministic selection/work lifecycle

Persist eligible-set digest, cursor/RNG facts, selected source revision, cooldown, typed work/artifact refs, cancellation/redaction. Generic receipts/audits retain strict restricted categories.

**Gate:** consent/veto/revocation/kill-switch race tests, identifier-leak validation, source deletion/crypto erasure, no raw facts in ordinary logs/DB/provider prompt.

**Commit:** `feat: preserve private inspiration safety on mongo`

## Phase 10 — Operations, cutover, and SQLectomy

### Task 10.1: Adapt recovery/operations tooling

**Files:**
- Rewrite/rename: `bin/database-ops.rs`, `bin/recovery-manifest.rs`, `bin/recovery-vault.rs`, `bin/source-vault.rs`, `bin/inspiration-admin.rs`
- Rewrite: `repository/operations.rs`

Implement Mongo/object/KMS-aware backup manifests, consistency audit, orphan asset scan, active-state recovery check, email key decrypt check, and restore drill. Keep secrets out of DB/manifests/logs.

Add DragonflyDB health check to startup: `PING` the DragonflyDB instance and log the result. If `DRAGONFLY_ENABLED=true` and the connection fails, log a warning and continue (the application degrades gracefully). Do not block startup on DragonflyDB availability. Verify that `FLUSHDB` on DragonflyDB does not cause data loss by running the two-account isolation suite with an empty cache.

### Task 10.2: Run complete two-account and failure matrix

Must include:

- anonymous/account A/account B/admin route/repository matrix;
- malicious opaque IDs and filter injection-shaped strings;
- revision/idempotency races;
- primary stepdown/transient transaction/unknown commit simulation;
- TTL delay;
- worker/provider/object-store failures;
- process restart mid-lobby/turn/encounter/generation/deletion;
- backup/restore with keys and external artifacts;
- validator/index drift.

Hosted mode remains fail-closed until this passes.

### Task 10.3: Remove PostgreSQL and legacy import

**Delete:**

- `migrations/0001_server_storage.sql` through `migrations/0031_custom_action_points.sql`;
- `scripts/postgres-roles.sql`;
- `scripts/check-postgres-role-policy.sh`;
- `crates/game-server/src/bin/legacy-import.rs`;
- obsolete SQL migration/database binaries or names;
- SQL-only repository modules after their Mongo replacements land.

**Modify:**

- root and server `Cargo.toml`: remove `sqlx` and PostgreSQL-only dependencies/features;
- `compose.yaml`: remove PostgreSQL service/volumes/health checks;
- config/docs/CI/scripts: remove `DATABASE_URL`, PostgreSQL role/migration assumptions;
- all 30 Rust files currently matching `sqlx::`, PostgreSQL types, or `PostgresRepository`.

Verification searches must return zero production matches:

```bash
rg 'sqlx::|PgPool|PgRow|PostgresRepository|DATABASE_URL' crates Cargo.toml compose.yaml scripts
find migrations -type f -name '*.sql'
```

Use repository search tools in agent execution if shell `rg/find` is unavailable.

### Task 10.4: Full verification

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

Also run:

- Mongo schema/index verify;
- BSON max-size tests;
- integration suite on replica set;
- two-account isolation suite;
- backup/restore drill;
- object metadata/digest/orphan scan;
- hosted startup fail-closed checks;
- `graphify update .` after code changes and inspect graph delta.

### Task 10.5: Documentation and release evidence

Update:

- run/development/production configuration docs;
- secret/key rotation runbook;
- backup/restore and deletion runbooks;
- schema/index bundle version notes;
- user-visible auth/session/privacy behavior;
- unsupported D&D mechanics/content tier.

Record test command outputs and deployment evidence in the project's configured workflow area. Do not claim production readiness from unit tests alone.

**Commit:** `refactor: remove postgres and complete mongo cutover`

## File-surface audit

At planning time, 30 Rust files contain SQLx/PostgreSQL repository coupling, including:

```text
crates/game-server/src/{application.rs,auth.rs,context.rs,error.rs,generation_ledger.rs,
  inspiration.rs,repository.rs,scene_images.rs}
crates/game-server/src/application/{hero,lifecycle,player_characters}.rs
crates/game-server/src/bin/{database-migrate,database-ops,legacy-import,recovery-manifest}.rs
crates/game-server/src/repository/{action_points,auth,governance,hero,images,inspiration,
  jobs,legacy,lifecycle,memberships,operations,pins,player_characters,presentations,recaps}.rs
```

There are 31 migration files plus `scripts/postgres-roles.sql`. Treat this list as a minimum audit surface, not an exhaustive guarantee; run final searches.

## Definition of done

The rewrite is complete only when:

1. All collections, validators, and named indexes reconcile and verify from a clean DB.
2. Every protected route/repository method requires a principal and applies ownership/membership/admin scope.
3. Raw secrets/tokens/email plaintext are absent from MongoDB and logs.
4. Signup access token, 24-hour signup session, credential login, roles, revoke, and expiry tests pass.
5. Character, campaign, enemy, event, play, encounter, GM, image, and BDE acceptance tests pass on a real replica set.
6. Snapshots are immutable and independent across campaigns/encounters.
7. Idempotency/concurrency/transaction-failure tests prove exactly-once behavior.
8. Active campaign deletion guard and cleanup manifest tests pass.
9. Private-inspiration safety/consent/veto/deletion tests pass.
10. Backup/restore restores DB, external assets, content/prompt pins, and keys—or intentionally revokes affected sessions—and consistency checks pass.
11. SQLx/PostgreSQL/legacy-import runtime dependencies and SQL migrations are gone.
12. Hosted mode passes the two-account isolation matrix and no longer needs to remain fail-closed.
13. If DragonflyDB is enabled, the application passes all tests with `FLUSHDB` on DragonflyDB (graceful degradation); session cache, throttle, and pub/sub fall through to MongoDB-only paths without user-facing failures.
