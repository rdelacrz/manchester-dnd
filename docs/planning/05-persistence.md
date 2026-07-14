# Persistence, data model, and versioning

## MVP storage decision

MVP uses SQLite through SQLx. It is appropriate for the initial single-deployment, turn-based workload and keeps local/private operation simple. PostgreSQL, an external queue, and object storage are scale options, not current dependencies.

The implemented persistence model is **revisioned JSON documents plus append-only audits**, not a full domain-event store:

| Current table | Role |
| --- | --- |
| `campaign_sessions` | Durable session/campaign document with schema version, optimistic revision, JSON payload, and timestamps |
| `characters` | Independently revisioned character document, optionally linked to a campaign session |
| `turn_audits` | Ordered append-only turn result/audit payload, unique by campaign session and turn number |
| `generated_assets` | Text/image artifact audit with provider, model, app-owned relative asset key, canonical SHA-256 prompt fingerprint, typed allowlisted metadata, and an optional same-campaign turn link |
| `command_receipts` | Bounded stored response and canonical request fingerprint keyed by campaign/idempotency key, linked to its same-campaign turn audit |

SQLite foreign keys, JSON validity checks, revision checks, and indexes are enabled in migrations. `SqliteRepository` atomically creates the initial campaign plus its exact declared character roster, then uses expected-revision compare-and-swap for audited event commits. Slice 1A atomically writes its session snapshot, `AbilityCheckResolved` audit, and command receipt; a receipt response is valid JSON bounded to 65,536 bytes. Preserve these contracts as the schema expands.

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
3. begin a bounded SQLite transaction and reload the campaign/character revisions;
4. return the prior response for a matching idempotency key before checking the now-stale revision, reject changed reuse of the key, otherwise reject a stale `expected_revision`;
5. run deterministic `game-core` validation/resolution with server-supplied dice;
6. serialize canonical validated documents and increment revisions atomically;
7. insert the immutable turn audit and any post-commit generation job in the same transaction;
8. commit, then return/notify the browser.

The implemented command gate prevents concurrent duplicates from consuming dice twice inside one process, while SQLite uniqueness is the persistence backstop. This is not yet a multi-instance write design. Never hold a SQLite write transaction open during a model/network call. Raw model proposals are revalidated against the locked revision. Narration/image failure cannot roll back a committed mechanical turn.

Use a WAL/synchronous policy selected and documented for the deployment, keep transactions short, and bound pool/worker concurrency. One application deployment should own writes in MVP. SQLite busy/locked classification and bounded retry remain pending in Slice 1A: repository failures currently fail closed and are not advertised as retryable. Before multi-writer use, classify only proven transient SQLite codes and preserve the same idempotency key for bounded retries.

## Document and audit rules

- Each serialized aggregate has an explicit schema version and stable `srd-5.1-cc` ruleset ID in or alongside its payload.
- Revisions are monotonic positive integers. Updates require the exact expected revision; a create requires absence.
- Turn audits are immutable after insert and include enough structured data to explain actor, intent, dice/modifiers, outcome, state delta, rules/content pins, and generation references.
- Administrative repair writes a new correction audit and revisioned document; it does not silently edit an old turn.
- Generated prose/images are presentation artifacts. Save the selected output and non-secret provenance; never regenerate it merely to load a session.
- Raw prompts are not stored in the normal repository. Prompt fingerprint, template/policy version, minimized input hashes, and provider/model configuration fingerprint are sufficient for routine provenance.
- Canonical serialization is required for hashes/exports. Rust memory/binary layout is never a durable format.

The current turn audit is not automatically a complete replay log. MVP save/resume reads the latest validated documents and uses audits for history/explanation. Do not promise full state reconstruction until every mutating fact is captured and replay tests prove it.

## Independent version axes

| Axis | Purpose |
| --- | --- |
| SQLite migration | Physical storage shape |
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

## SQLite operations and recovery

- Store the database on a persistent local volume with least-privilege file/directory permissions; never place it under public/static roots.
- Use SQLite's supported online backup API or a coordinated checkpoint/backup procedure. Do not assume copying a live WAL database file alone is consistent.
- Encrypt host/volume backups; document whether application-level encryption is required for particularly sensitive fields.
- Track migration version, database/WAL size, write latency, busy/locked errors, integrity-check results, backup age, and restore-test result.
- `GET /health/ready` currently checks pool/query connectivity with `SELECT 1`; it does not prove writeability, free disk space, backup health, or provider availability. `GET /health/live` has no dependency check.
- Test abrupt process termination around save, expired job lease reclamation, duplicate idempotency keys, corrupt/unknown JSON schema, foreign-key violation, disk full, and backup restoration.
- Deletion/retention documentation covers live database, generated files, diagnostics, exports, and backup expiry.

## Evolution: event stream and PostgreSQL

A complete append-only domain-event stream with periodic snapshots is an ambitious later step if branching replay, audit depth, multiplayer, or complex migrations justify it. Introduce it through dual-write verification and state-equivalence tests; do not relabel `turn_audits` as complete events without proving coverage.

Migrate from SQLite to PostgreSQL only when measured concurrency, multi-instance availability, managed backup, or operational requirements demand it. Before cutover:

1. define repository contract tests that run against both backends;
2. avoid backend-specific JSON/time/locking assumptions in durable DTOs;
3. build and verify export/import with row counts, hashes, foreign keys, and sampled loaded state;
4. rehearse rollback/read-only windows and preserve IDs/revisions;
5. then introduce least-privilege database roles and independently scaled workers/object storage.

Consent/source handling is specified in [06-consent-privacy-safety](06-consent-privacy-safety.md); broader scale boundaries are in [architecture](02-architecture.md).
