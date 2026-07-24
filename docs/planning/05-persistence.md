# Persistence, concurrency, export, and recovery

## Storage decision

MongoDB 8 is the only authoritative application database. It runs as a replica set
so multi-document writes can use transactions. DragonflyDB is an optional cache,
throttle fast path, invalidation channel, and pub/sub transport; its loss must never
lose durable state or weaken authorization.

The model is revisioned domain documents plus immutable audits and exact command
receipts—not a complete event-sourced store. Thirty-four managed collections cover
accounts/sessions, campaigns/memberships, character library and campaign instances,
play/encounter events, generation, artifacts, inspiration consent, deletion, and
schema metadata.

`crates/game-server/src/persistence/` is the schema catalog. Every managed collection
has a validator and bounded indexes. `mongo-admin schema apply` reconciles the full
bundle under a schema-admin credential; the app credential cannot mutate schema.

## Identity and tenant boundary

- Accounts, sessions, invitations, campaigns, characters, jobs, artifacts, and
  receipts use opaque bounded IDs.
- Email is stored encrypted; normalized lookup uses a separately keyed HMAC.
- Every user-visible query includes owner/account/campaign membership scope.
- Character-library records contain no level, XP, HP, slots, or runtime state.
  Campaign-specific character instances own progression and runtime values.
- Raw private-inspiration Markdown never enters MongoDB or DragonflyDB; only bounded
  reviewed facts, opaque IDs, consent state, and digests do.

## Command lifecycle

1. Authenticate the MongoDB-backed opaque session and validate CSRF/origin policy.
2. Load campaign membership/ownership and the expected authoritative revision.
3. Interpret any free-form input into a bounded proposal before opening a write
   transaction or calling deterministic mechanics.
4. Start a short MongoDB transaction and re-read authoritative documents with tenant
   filters.
5. Resolve exact idempotency-key replay before stale-revision rejection; reject the
   same key with a changed request fingerprint.
6. Run deterministic `game-core` validation/resolution with server-owned dice/time.
7. Compare-and-swap revisions and atomically write state, immutable audit/event, and
   exact command receipt.
8. Commit, then update/invalidate Dragonfly opportunistically and return the response.

Transactions never span model/provider calls. Whole-transaction retry is bounded to
MongoDB's explicit transient-transaction label. Unknown commit results retry commit,
not business logic. Retries preserve the original idempotency key, expected revision,
randomness cursor, and provider result.

## Document and audit rules

- DTOs deny unknown fields and validate schema version, IDs, bounded strings/lists,
  revisions, pins, and state invariants before persistence.
- `schema_version` describes a durable document contract; managed schema-bundle
  version/digest describes physical validators and indexes.
- Immutable audits contain bounded canonical facts, actor/scope/category/action,
  revisions, and timestamps. Secrets, raw prompts, emails, provider credentials, and
  generated binary bodies are forbidden.
- Command receipts retain the exact canonical response needed for lost-response
  recovery and reject changed reuse.
- TTL indexes remove expired signup/session/throttle/draft/reservation/quarantine/
  deletion records only where MongoDB TTL semantics are safe; authoritative domain
  retention still uses explicit state transitions.

## Save, resume, history, archive, and delete

Resume loads exact pinned rules/content/theme/prompt/schema fingerprints and derives
legal current actions from canonical state. History is a deterministic projection of
ordered immutable events/audits and hides internal payloads not authorized for the
viewer.

Archive is reversible and blocks mutation until explicit restore. Deletion is a
two-stage prepare/confirm flow with short-lived confirmation evidence, dependent
cleanup, and longer opaque tombstones so restored backups reapply deletion.

## Export and restore

Two separate formats exist:

- **Private readable export:** owner-authorized, human-readable, excludes secrets,
  credentials, private source material, cache entries, and another user's consent.
- **Canonical recovery/export:** versioned, integrity-bound state suitable for exact
  restore after authorization, schema/pin validation, and conflict checks.

Exports use stable ordering and canonical JSON/digests. Restore never treats prose or
AI output as authority and never imports Dragonfly data.

## Schema and content versioning

| Axis | Purpose |
| --- | --- |
| MongoDB schema bundle | Validators, indexes, collection options |
| Durable document schema | Meaning/shape of each document/audit |
| Ruleset version | Mechanical profile |
| Content/theme digest | Exact immutable authored material |
| Prompt/policy digest | Exact generation policy |
| Capability fingerprint | Reachable mechanic support |

Campaigns pin all relevant axes. New greenfield data uses only current pins; obsolete
persistence-engine import paths are not retained.

## Operations and recovery

- Use separate secret-managed application and schema-admin credentials.
- Require authentication, replica-set semantics, and certificate-validated TLS
  outside loopback; reject administrative database names.
- Monitor connections, transaction aborts/retries, majority-commit latency, replication
  health/lag, validation failures, index use, job lease age, disk, and backup freshness.
- Use `mongodump --archive --gzip` for portable logical recovery evidence and provider
  snapshots/PITR when the required RPO is shorter than logical-backup intervals.
- Encrypt backups with the separate `recovery-vault` key and include protected RNG/
  image state. Never include Dragonfly.
- Restore into a separate random database, verify the complete schema catalog, compare
  document counts/manifests, and exercise representative load/resume flows before
  cutover.

The executable procedure is
[`docs/operations/database-recovery.md`](../operations/database-recovery.md).

## Scale evolution

Scale only from measurements:

1. keep repository contracts and restore tests independent of hosting provider;
2. move MongoDB to a managed replica set/sharded topology when availability, size,
   or throughput requires it;
3. add stale-tolerant read models only where immediate post-commit consistency is not
   required;
4. split workers/object storage/external queues only after measured job/artifact
   pressure;
5. keep MongoDB as authority and Dragonfly disposable at every topology.
