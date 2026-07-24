# MongoDB reliability and recovery runbook

MongoDB is Manchester Arcana's only system of record. DragonflyDB is an optional,
disposable cache/pub-sub layer: never back it up, never restore it as authoritative
state, and always allow the application to reconstruct it from MongoDB.

Generated scene-image bytes and the RNG master key are protected filesystem state.
A usable recovery unit contains the MongoDB dump plus those files.

## Recovery objective

- RPO is the latest completed and authenticated recovery vault. Create one before
  deletion, schema changes, host replacement, or other destructive work.
- RTO is measured by `scripts/run-database-recovery-drill.sh`; no external SLO is
  claimed until representative campaign/artifact volumes have repeated evidence.
- Encrypted vaults expire at exactly 30 days. Opaque deletion tombstones remain for
  35 days so every still-live backup can replay deletion before use.
- A named Compose volume is not a backup. A backup counts only after authenticated
  open, isolated restore, schema verification, and document-count comparison.

## Roles and credentials

The Compose bootstrap creates two MongoDB roles:

| Credential | Intended access |
| --- | --- |
| Application | Read/write existing managed collections; no schema mutation. |
| Schema administrator | `dbOwner` for the configured database; used only by `mongo-admin schema apply`. |

The Compose root credential is development/test-only. Production credentials must
come from a secret manager. Do not place passwords in shell history, process
arguments, logs, evidence, or repository files.

Verify least privilege after bootstrap:

```sh
MONGODB_APP_URI="$APP_MONGODB_URI" \
MONGODB_SCHEMA_URI="$SCHEMA_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
  scripts/check-mongo-role-policy.sh
```

Apply and verify the managed schema separately from application startup:

```sh
MONGODB_URI="$APP_MONGODB_URI" \
MONGODB_SCHEMA_URI="$SCHEMA_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
  cargo run --locked -p manchester-dnd-server --bin mongo-admin -- schema apply

MONGODB_URI="$APP_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
  cargo run --locked -p manchester-dnd-server --bin mongo-admin -- schema verify
```

The application role must not create collections, change validators/indexes, manage
users/roles, or administer the server. Rotate by creating a replacement user with
the same narrow role, testing readiness plus an idempotent read, switching the
secret, restarting, then revoking the old user.

Hosted MongoDB URIs must authenticate, name a replica set (or use `mongodb+srv`),
and require certificate-validated TLS. Local plaintext is accepted only on
loopback. The configured database name is narrowly allowlisted and may not target
`admin`, `config`, or `local`.

## Transactions, consistency, and retry policy

- MongoDB must run as a replica set; multi-document mutations use transactions.
- Transactions remain short and never span provider/network calls.
- Tenant and owner/member filters are present in every authoritative query.
- Expected revisions and durable idempotency keys resolve ordinary concurrency.
- The driver retries a whole transaction only for the explicit transient transaction
  label. Unknown commit results retry commit, not business logic. Randomness and
  provider calls are never regenerated during retry.
- Duplicate-key errors are mapped only for known indexes/contracts; all other
  persistence failures remain internal and redacted.

Dragonfly failures degrade to cache misses/no-op publication. They must never grant
access, extend a session, bypass MongoDB expiry/revocation, or prevent an
authoritative MongoDB write.

## Recovery unit and authenticated vault

`recovery-vault` uses a separate random 256-bit key and a versioned chunked
XChaCha20-Poly1305 format. Header, length, digest, chunk index, and chunk length are
authenticated. Outputs use mode `0600`; symlinks, wrong keys, tampering, trailing
bytes, partial output, and existing destinations fail closed.

A recovery bundle contains:

- compressed MongoDB archive with collection options, validators, and indexes;
- exact RNG master key when present;
- protected generated-image tree when present;
- deterministic recovery manifest and body-free operational snapshot;
- an explicit marker that DragonflyDB is excluded.

Keep `.runtime-private/keys/recovery-vault.key` off-host or in a separately protected
secret store. Losing the key loses the backup; storing it beside the only vault
defeats host-loss recovery.

## Non-destructive backup/restore drill

Stop mutating web/worker processes or establish an operator-confirmed quiescent
window, then run:

```sh
MONGODB_TEST_URI="$LOOPBACK_ROOT_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
RNG_MASTER_KEY_FILE=/secure/manchester-arcana/rng-master.key \
IMAGE_ARTIFACT_ROOT=/secure/manchester-arcana/generated-images \
RECOVERY_VAULT_KEY_FILE=/secure/offline/manchester-arcana-recovery.key \
RECOVERY_BACKUP_ROOT=/secure/backups/manchester-arcana \
RECOVERY_EVIDENCE_ROOT=/secure/evidence/manchester-arcana \
  scripts/run-database-recovery-drill.sh
```

The drill:

1. applies and verifies the current managed MongoDB schema;
2. records a body-free recovery manifest and operational snapshot;
3. creates a compressed MongoDB archive;
4. bundles protected files, explicitly excludes DragonflyDB, and seals the bundle;
5. authenticates, opens, and byte-compares the encrypted vault;
6. restores into a uniquely named isolated MongoDB database;
7. verifies all managed validators and indexes;
8. requires every source/restored collection document count to match;
9. drops the isolated database in an exit trap and retains evidence separately.

The source database and encrypted vault are never overwritten. Any mismatch fails
the drill.

Expire vaults only after confirming both path and key:

```sh
cargo run --locked -p manchester-dnd-server --bin recovery-vault -- \
  expire /secure/backups/manchester-arcana \
  /secure/offline/manchester-arcana-recovery.key \
  "$(date -u +%s)"
```

## Operational snapshot

```sh
MONGODB_URI="$OPERATOR_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
  cargo run --locked -p manchester-dnd-server --bin database-ops

df -Pk /path/containing/mongodb-and-protected-artifacts
```

The JSON snapshot is body-free. Readiness proves authoritative MongoDB access;
Dragonfly health is reported separately and is not a prerequisite for safe reads or
writes.

## Schema failure and read-only recovery

Managed schema changes are versioned as a complete validator/index bundle. Never
manually edit only part of that bundle.

If schema application or verification fails:

1. stop mutating application/worker processes;
2. preserve redacted diagnostics and the original database;
3. create and authenticate a fresh recovery vault when reads remain possible;
4. fix forward in the schema catalog, or restore the last compatible vault into a
   separate database;
5. run exact schema/document verification before changing `MONGODB_DATABASE` or URI;
6. keep the original database until load/resume and a new recovery drill pass.

## Disk-full, transaction, and corruption response

- **Low disk:** stop optional workers, then the app. Do not delete MongoDB journal/
  data files, audits, selected artifacts, RNG keys, or the only backup. Expand/free
  space outside authoritative storage, then run a fresh recovery drill.
- **Transient transaction conflict:** retain the same idempotency key and expected
  revision. Let only the bounded driver transaction policy retry.
- **Validation/corrupt BSON:** stop mutation, archive the database read-only, and
  reproduce against an isolated restore. Fix forward; never rewrite immutable audit
  history in place.
- **Lost connection/process death:** MongoDB aborts an uncommitted transaction.
  Reload and use the same idempotency key. A committed receipt/audit wins; absence
  means no partial command is authoritative.
- **Dragonfly outage/data loss:** do not restore cache files. Restart or flush the
  service and let cache/session/pub-sub entries repopulate from MongoDB.
