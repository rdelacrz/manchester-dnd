# Persistence, data model, and versioning

## MVP storage decision

MVP uses PostgreSQL through SQLx. It provides row-level locks, concurrent connection handling, transactional constraints, and a direct path from local development to multiple application/worker processes without changing database engines. Local development uses the Compose service; deployments may use a managed PostgreSQL service. An external queue and object storage remain scale options, not current dependencies.

The implemented persistence model is **revisioned JSON documents plus append-only audits**, not a full domain-event store:

| Current table | Role |
| --- | --- |
| `campaign_sessions` | Durable session/campaign document with schema version, optimistic revision, JSON payload, and timestamps |
| `characters` | Independently revisioned character document, optionally linked to a campaign session |
| `turn_audits` | Ordered append-only turn result/audit payload, unique by campaign session and turn number |
| `generated_assets` | Text/image artifact audit with provider, model, app-owned relative asset key, canonical SHA-256 prompt fingerprint, typed allowlisted metadata, and an optional same-campaign turn link |
| `command_receipts` | Bounded stored response and canonical request fingerprint keyed by campaign/idempotency key, linked to its same-campaign turn audit |
| `hero_creation_drafts` | Resumable local-owner creation document with an expiry and a separate retention deadline |
| `hero_characters` | Authoritative created-hero document, unique per campaign owner |
| `hero_audits` | Ordered append-only creation, trusted-reward, and level-up audits |
| `hero_command_receipts` | Bounded exact-response receipts for creation and advancement commands |

PostgreSQL foreign keys, JSONB columns, revision checks, digest/response constraints, and indexes are enabled in migrations. `PostgresRepository` atomically creates the initial campaign plus its exact declared character roster, then combines `SELECT ... FOR UPDATE` row locks with expected-revision compare-and-swap for audited event commits. Slice 1A atomically writes its session snapshot, `AbilityCheckResolved` audit, and command receipt; a receipt response preserves its canonical JSON text and is bounded to 65,536 bytes. Preserve these contracts as the schema expands.

Terminology in current code uses **campaign session** for the durable game. If a later product adds “play sessions” (one sitting within a campaign) and browser authentication sessions, rename/version DTOs carefully rather than using one ambiguous `session` field for all three.

## MVP additions

Add tables only when a delivered slice uses them. `command_receipts` is now implemented for the exploration-check path. Remaining likely additions are:

- `generation_jobs` and attempts for crash-durable asynchronous text/image work;
- content-pack/source-version pins and campaign safety/progression settings;
- consent/eligibility metadata using opaque private-source IDs, never raw Markdown by default;
- browser/account records only if the deployment choice is hosted rather than explicit local single-user;
- export/archive/deletion state and a minimal security/consent audit.

Keep normalized, query-critical metadata in columns; keep versioned aggregate state in validated JSON payloads. Do not turn every model attempt or operational log into game state.

## Authoritative save transaction

The current `commit_session_event` baseline atomically advances a validated session snapshot, any XP/level character snapshot, and its immutable audit row. It rejects stale revisions, skipped event sequences, mismatched XP summaries, arbitrary session metadata rewrites, and standalone character/session updates. Other mechanical character transitions must gain a typed event plus transaction validation before they are implemented; they must not reuse the XP path.

`GameApplicationService` now implements this sequence for the one authored local exploration check. Other gameplay mutations must pass through the same boundary before they are exposed:

For a state-changing turn:

1. authenticate/authorize or enforce declared local mode;
2. parse any free-form intent against a safe view before acquiring a write lock;
3. begin a bounded PostgreSQL transaction, lock the campaign and affected characters in deterministic order, and reload their revisions;
4. return the prior response for a matching idempotency key before checking the now-stale revision, reject changed reuse of the key, otherwise reject a stale `expected_revision`;
5. run deterministic `game-core` validation/resolution with server-supplied dice;
6. serialize canonical validated documents and increment revisions atomically;
7. insert the immutable turn audit and any post-commit generation job in the same transaction;
8. commit, then return/notify the browser.

The implemented command gate prevents concurrent duplicates from consuming dice twice inside one process. PostgreSQL row locks, unique constraints, expected revisions, and the receipt primary key are the cross-process persistence backstop: concurrent writers cannot commit the same revision twice, and the losing writer observes the committed current revision. A dedicated race test exercises this with independent repository handles. Full multi-instance duplicate-request replay still requires an application-level idempotency reservation/recheck so a losing process does not consume throwaway randomness before discovering the winner's receipt.

Never hold a PostgreSQL transaction or row lock open during a model/network call. Keep transactions short, acquire campaign then character locks in a stable order, and bound application/worker pool concurrency. Repository failures currently fail closed and are not advertised as retryable. Before automatic retries, classify only proven transient SQLSTATEs such as serialization failure or deadlock, add bounded jittered backoff, and preserve the original expected revision and idempotency key so retry cannot reroll or duplicate an audit.

## Document and audit rules

- Each serialized aggregate has an explicit schema version and stable `srd-5.1-cc` ruleset ID in or alongside its payload.
- Durable row revisions are monotonic positive integers. Hero-domain command revisions start at zero, so a hero row stores `payload revision + 1`; every update still requires the exact expected domain revision and advances both forms once. A create requires absence.
- Turn audits are immutable after insert and include enough structured data to explain actor, intent, dice/modifiers, outcome, state delta, rules/content pins, and generation references.
- Administrative repair writes a new correction audit and revisioned document; it does not silently edit an old turn.
- Generated prose/images are presentation artifacts. Save the selected output and non-secret provenance; never regenerate it merely to load a session.
- Raw prompts are not stored in the normal repository. Prompt fingerprint, template/policy version, minimized input hashes, and provider/model configuration fingerprint are sufficient for routine provenance.
- Canonical serialization is required for hashes/exports. Rust memory/binary layout is never a durable format.

The current turn audit is not automatically a complete replay log. MVP save/resume reads the latest validated documents and uses audits for history/explanation. Do not promise full state reconstruction until every mutating fact is captured and replay tests prove it.

## Independent version axes

| Axis | Purpose |
| --- | --- |
| PostgreSQL migration | Physical storage shape |
| Campaign/character/turn payload schema | Meaning/shape of each JSON document/audit |
| Ruleset ID/version | Mechanical profile, initially `srd-5.1-cc` |
| Core mechanic/RNG build version | Reproduction and compatibility of deterministic outcomes |
| Content pack ID/version/digest | Character, creature, theme, and presentation definitions |
| Private prompt parser/source digest | Interpretation of installed Markdown inspiration |
| Prompt template and safety-policy version | Generation behavior and permitted context |
| Model configuration fingerprint | Non-secret backend/model/parameter identity |

Do not collapse these into one application version. A loaded document must either validate under a supported schema or produce a typed incompatible-version error; it must never guess missing semantics.

Content/rules changes do not mutate old documents in place. A campaign migration first performs a dry run, reports changed/unsupported mechanics, preserves an export, then writes a new revision with explicit migration audit after owner confirmation. SRD 5.1 state is never silently treated as SRD 5.2.1.

## Save, resume, history, and export

MVP must provide:

- autosave on every committed turn with visible saved/pending/conflict status;
- safe resume from the latest campaign/character revisions after browser/server restart;
- ordered turn history and roll explanation from audits;
- a player-readable export with character sheet, campaign state/summary, turns, dice, selected generated artifacts, rules/content pins, and attribution;
- a machine-readable canonical export of all restorable MVP documents/audits, excluding credentials and raw private source files;
- explicit archive/deletion behavior and an operator-tested restore path.

Slice 1A currently proves a narrower path: page load lazily creates or resumes the fixed local campaign, reload reads validated documents plus ordered audits, and the latest stored ability-check dice and outcome are projected without rerolling. It is not full event-sourced reconstruction, campaign export, archive/delete, or a process-restart/browser-E2E claim.

A public/shareable recap is later and must be a separately authorized redacted projection. A private export must not include another person's consent record or source Markdown.

## PostgreSQL operations and recovery

- Use separate least-privilege application, migration, backup, and operator roles where deployment tooling permits. The application role must not create databases, roles, or extensions; SQLx tests use a separate development/CI role with temporary-database privileges.
- Require encrypted connections whenever database traffic leaves the host or trusted private boundary. Treat `DATABASE_URL` as a credential, redact it from debug/error output, and rotate it through the deployment secret system.
- Bound web and worker connection pools against the server connection budget. Monitor pool wait time, active/idle connections, transaction duration, row-lock waits, deadlocks, statement latency, and revision-conflict rate.
- Use `pg_dump`/`pg_restore` for logical portability and a provider-supported physical/base-backup plus WAL/PITR strategy when the recovery-point objective requires it. Encrypt backups, set finite retention, and test restore into an isolated database.
- Track migration version, database/index size, WAL growth, autovacuum/analyze health, replication lag when applicable, disk capacity, backup age, and restore-test result.
- `GET /health/ready` currently checks pool/query connectivity with `SELECT 1`; it does not prove writeability, free disk space, backup health, or provider availability. `GET /health/live` has no dependency check.
- Test concurrent writers, abrupt process termination around save, expired job lease reclamation, duplicate idempotency keys, deadlock/serialization handling once retry exists, corrupt/unknown JSON schema, foreign-key violation, disk full, migration rollback, and backup restoration.
- Deletion/retention documentation covers live database, generated files, diagnostics, exports, and backup expiry.

## Legacy local-data cutover

An existing embedded-database file is legacy input, not a supported runtime backend. Do not delete or mutate it during the backend transition. Before retiring one:

1. make a read-only backup and record its digest;
2. export through a pinned version of the previous application/schema into a bounded, versioned interchange format;
3. import into an isolated PostgreSQL database using an explicit, repeatable tool rather than ad hoc SQL;
4. validate table/document counts, IDs, revisions, ordered audits, foreign-key relationships, canonical state hashes, and generated-asset references;
5. run representative load/resume/history flows against the imported data;
6. retain the source backup and import report until the rollback window expires.

The import must be idempotent or fail before partial publication. Cut over the application only after validation passes; rollback restores the pre-cutover configuration and leaves the source backup untouched. If no user-authored campaign exists, record that evidence and initialize a fresh PostgreSQL database rather than manufacturing an empty import.

## Evolution: event stream and PostgreSQL topology

A complete append-only domain-event stream with periodic snapshots is an ambitious later step if branching replay, audit depth, multiplayer, or complex migrations justify it. Introduce it through dual-write verification and state-equivalence tests; do not relabel `turn_audits` as complete events without proving coverage.

PostgreSQL is the only supported application database, so there is no dual-backend compatibility layer or later engine cutover. Scale its topology only from measured evidence:

1. keep repository contract, concurrency, migration, export/import, and restore tests independent of a particular hosting provider;
2. add a connection proxy only when pool/connection measurements require one;
3. add read replicas only for explicitly stale-tolerant read models, never authoritative revision checks or immediate post-commit reads;
4. partition audits/jobs only after table/index measurements identify a real maintenance or query problem;
5. independently scale workers or introduce object storage/external queues only after PostgreSQL job, artifact, or egress pressure justifies it.

Consent/source handling is specified in [06-consent-privacy-safety](06-consent-privacy-safety.md); broader scale boundaries are in [architecture](02-architecture.md).
