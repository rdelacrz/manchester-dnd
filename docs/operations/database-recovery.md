# PostgreSQL reliability and recovery runbook

This runbook is for Manchester Arcana's supported local single-user profile.
PostgreSQL is authoritative. Generated scene-image bytes and the RNG master key
are protected filesystem state and form one recovery unit with the logical dump.
The old SQLite file is read only by an explicitly feature-gated offline importer;
the game server has no SQLite runtime backend.

## Recovery objective

The private MVP has a manual, owner-triggered backup objective:

- RPO is the latest completed, authenticated recovery vault. Make a fresh vault
  before deletion, migration, host replacement, or other destructive work. There
  is no honest time-based RPO while backups remain manual.
- RTO is not yet an external SLO. Each drill records elapsed operator evidence;
  set a time target only after representative campaign/artifact volumes have
  several private-test measurements.
- Encrypted vaults expire at exactly 30 days. Opaque deletion tombstones live 35
  days so a restore from any still-live backup can reapply deletion before use.
- Logical `pg_dump`/`pg_restore` is sufficient for that objective. A hosted
  deployment, a database larger than the measured logical-restore window, or an
  RPO shorter than the backup interval requires provider-supported base backups,
  continuous WAL archiving, and an isolated point-in-time recovery drill before
  the profile can be enabled.

The named Compose volume is not a backup. Never count a dump that has not been
authenticated, opened, and restored into a separate database.

## Roles and credentials

`scripts/postgres-roles.sql` creates fixed `NOLOGIN` groups:

| Group | Intended access |
| --- | --- |
| `manchester_arcana_migration` | Owns the public schema and durable objects; creates/alters schema only. |
| `manchester_arcana_app` | DML on game tables; cannot create roles/databases/schema or rewrite recovery evidence. The local web process and in-process worker share this role. |
| `manchester_arcana_backup` | Read-only table access for `pg_dump` and deterministic recovery manifests. |
| `manchester_arcana_operator` | `pg_monitor`, body-free queue/migration/recovery status, and narrow recovery-status updates; no campaign-table read. |

Create credential-bearing `LOGIN` roles through the deployment secret system,
grant each exactly one group, and rotate the login rather than changing a group.
Do not put a password in this repository, shell history, process argument, dump,
or evidence file. The local Compose user is a development-only combined
administrator; it is the documented exception used by isolated tests and drills.

After initial migrations, apply and verify the policy:

```sh
scripts/check-postgres-role-policy.sh
```

For a separated deployment, run embedded migrations with the migration login,
then start the application with migration disabled:

```sh
DATABASE_URL="$MIGRATION_DATABASE_URL" \
  cargo run --locked -p manchester-dnd-server --bin database-migrate

DATABASE_MIGRATE_ON_START=false \
DATABASE_URL="$APP_DATABASE_URL" \
  target/release/manchester-dnd-web
```

The migration login must `SET ROLE manchester_arcana_migration` (or have object
ownership through that group) before running migrations. Never give the app role
`CREATEDB`, `CREATEROLE`, superuser, replication, bypass-RLS, or extension-creation
authority.

Use a secret-managed PostgreSQL URL. Loopback-only Compose may use plaintext
inside the host boundary. Any TCP hop beyond loopback/private same-host namespace
must use certificate-validated TLS (`sslmode=verify-full`, trusted CA, matching
server name); do not accept `prefer`, an unverified certificate, or a public
database listener. Rotate by creating a new login, granting the same group,
testing readiness plus one idempotent read, switching the secret, restarting,
then revoking and dropping the old login. Re-run the role-policy check and a
recovery manifest afterward.

## Pool, transaction, and lock policy

The app validates these controls at startup:

| Setting | Default | Valid range |
| --- | ---: | ---: |
| `DATABASE_MAX_CONNECTIONS` | 10 | 1–50 |
| `DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS` | 5,000 | 1–60,000 |
| `DATABASE_STATEMENT_TIMEOUT_MILLISECONDS` | 30,000 | 1–300,000 |
| `DATABASE_LOCK_TIMEOUT_MILLISECONDS` | 5,000 | 1–60,000 |
| `DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS` | 15,000 | 1–300,000 |

Budget the database's `max_connections` across web replicas, worker replicas,
migration/backup/operator sessions, and a reserved emergency margin. Do not set
each process to the database maximum. Every pool connection sets `read committed`,
the statement/lock/idle-in-transaction timeouts above, and an application name.
Canonical export upgrades only its short transaction to repeatable-read/read-only.

Transactions remain short and never span a provider/network call. Mutation code
locks the campaign before dependent hero/character/job/artifact rows; multi-row
collections use deterministic identifier order. Expected revisions and durable
idempotency keys resolve ordinary concurrency. The closed transient allowlist is
only PostgreSQL `40001` (serialization failure) and `40P01` (deadlock detected).
The private MVP performs zero automatic database retries. A future bounded retry
must replay the whole transaction with the original expected revision and
idempotency key; it may not regenerate randomness, call a provider again, or
reuse a half-completed transaction. Lock timeout, query cancellation, connection
loss, uniqueness/foreign-key failure, and internal errors are not automatically
classified as safe retries.

## Recovery unit and authenticated vault

`recovery-vault` uses a separate random 256-bit key and a distinct, versioned
chunked XChaCha20-Poly1305 format. Header, length, digest, chunk index, and chunk
length are authenticated. Outputs are created with mode `0600`; symlinks,
wrong-key data, tampering, trailing bytes, partial output, and an existing
destination fail closed. The ordinary game/image process never loads this key.

One recovery bundle contains:

- PostgreSQL custom-format dump;
- exact 32-byte RNG master key;
- protected generated-image tree;
- deterministic source recovery manifest and file checksums;
- the legacy SQLite file while it remains retained.

Keep `.runtime-private/keys/recovery-vault.key` off the application host or in a
separately protected secret store for a real deployment. Losing the key loses the
backup; storing it beside the only vault defeats host-loss recovery. Rotate by
creating a new key, sealing and verifying a fresh current recovery unit, retaining
the old key only until every old-key vault expires, then destroying it.

## Non-destructive backup/restore drill

Stop the app and image/text workers, or otherwise establish an operator-confirmed
quiescent window. The script never restores over the source database:

```sh
DATABASE_URL="$BACKUP_DATABASE_URL" \
RNG_MASTER_KEY_FILE=/secure/manchester-arcana/rng-master.key \
IMAGE_ARTIFACT_ROOT=/secure/manchester-arcana/generated-images \
RECOVERY_VAULT_KEY_FILE=/secure/offline/manchester-arcana-recovery.key \
RECOVERY_BACKUP_ROOT=/secure/backups/manchester-arcana \
RECOVERY_EVIDENCE_ROOT=/secure/evidence/manchester-arcana \
  scripts/run-database-recovery-drill.sh
```

The drill:

1. applies only embedded forward migrations through the explicit migration tool;
2. computes a body-free manifest of migration versions, campaign revisions,
   ordered audit/export state hashes, pins, selected artifacts, and RNG digest;
3. creates and validates a custom-format dump;
4. bundles protected files and checksums, seals the bundle, and proves the dump
   header is absent from ciphertext;
5. opens and checksum-validates the authenticated bundle;
6. creates a uniquely named isolated database and restores with
   `--exit-on-error --no-owner --no-acl`;
7. recomputes the manifest against the restored database/files and requires an
   exact match;
8. proves the vault exists one second before the 30-day boundary and expires at
   the boundary;
9. records body-free backup/restore health and disk capacity, then drops the
   isolated database in an exit trap.

Any mismatch fails the drill and records a failed restore result when a vault
receipt exists. The source database and vault remain untouched. Inspect
`drill-summary.json`, both manifests, the recovery receipt, PostgreSQL version,
disk capacity, and database operations snapshot before signing off. Move the
vault off-host and retain the evidence separately; the default `.runtime-private`
location is for local drills and is ignored by Git.

Expire real vaults only after confirming the key and directory are correct:

```sh
cargo run --locked -p manchester-dnd-server --bin recovery-vault -- \
  expire /secure/backups/manchester-arcana \
  /secure/offline/manchester-arcana-recovery.key \
  "$(date -u +%s)"
```

The expiry command accepts only authenticated `.marv` files in a flat real
directory. A forged timestamp cannot trigger deletion.

## Operational snapshot

Run with the operator login (the development Compose administrator is the local
exception):

```sh
DATABASE_URL="$OPERATOR_DATABASE_URL" \
  cargo run --locked -p manchester-dnd-server --bin database-ops
df -Pk /path/containing/postgres-and-protected-artifacts
```

The JSON snapshot contains no campaign bodies. It reports latest migration,
database/index/WAL bytes (WAL may be unavailable without `pg_monitor`), connection
and wait counts, transactions older than 30 seconds, oldest transaction, lock
waits, cumulative deadlocks and block I/O time, never-analyzed/dead-tuple health,
replication clients/lag where configured, generation queue/lease age, and last
backup/restore result. The recovery drill separately records filesystem capacity.
Readiness remains intentionally narrower: it proves `SELECT 1`, not free disk,
backup freshness, writeability, provider health, or recovery fitness.

Do not invent alert thresholds from an empty local database. After representative
private runs, graph bounded snapshots, establish baseline percentiles, and alert
on sustained user-impact signals: failed turn commits, pool acquisition timeouts,
growing lock waits/long transactions, old runnable jobs, expired leases, old or
failed backup evidence, migration drift, or low disk headroom.

## Migration failure and read-only recovery

Never edit a migration already used outside a disposable test database. Before a
schema change, complete a recovery drill. Run migrations separately, then start
the app with `DATABASE_MIGRATE_ON_START=false`. If migration fails:

1. keep the old application stopped or in operator-enforced read-only mode;
2. preserve redacted diagnostics and make no manual partial schema edits;
3. keep the original database and backup;
4. fix forward with a new migration, or restore the last compatible vault into a
   different database;
5. run the exact manifest comparison before changing `DATABASE_URL`;
6. keep the old database until application load/resume and a new backup pass.

The app has no writable HTTP “maintenance mode.” Read-only operation means keeping
the mutating app stopped and using the backup/operator roles for verified export
and inspection. Do not start an old binary against a schema it does not support.

## Disk-full, lock, and corruption response

- Low disk: stop workers first, then the app; do not delete WAL, PostgreSQL files,
  audits, current artifacts, or the only backup. Free space outside authoritative
  storage or expand the volume. Make a fresh dump after PostgreSQL is healthy and
  run the restore drill before resuming writes.
- Deadlock/serialization: retain the original request key and expected revision.
  The current app returns failure/reload rather than retrying. Inspect body-free
  correlation/audit IDs and lock waits; do not rerun a provider or roll manually.
- Constraint/corrupt JSON: stop mutation, make a read-only dump, preserve the
  immutable audit, and reproduce in an isolated database. Fix forward through a
  reviewed migration/correction audit; never edit history in place.
- Lost connection or abrupt process death: PostgreSQL rolls back the open
  transaction. Reload the campaign and use the same idempotency key. A committed
  receipt/audit wins; absence means no partial command is authoritative.
- Expired generation lease: the worker closes the old attempt body-free and
  reclaims through the tested lease path. Do not update job rows by hand.

## Legacy SQLite import and rollback

The retained `data/manchester-arcana.db` has supported migrations 1 and 2. It
contains one campaign and one baseline character, with no turns, receipts, or
assets. It is not authoritative for the running game and is not deleted.

The offline importer is excluded from normal server builds and requires the
explicit `legacy-import` Cargo feature. It rejects sidecar/WAL files, a bad source
digest, failed/unknown migrations, invalid domain rows, broken links, count/hash
drift, timestamp drift, and any non-identical target. All publication occurs in
one serializable PostgreSQL transaction. Exact replay inserts zero rows.

Rehearse the encrypted backup, first import, replay, canonical state hashes, and
rollback without touching the source:

```sh
scripts/run-legacy-import-drill.sh
```

For a real cutover, first stop every legacy writer, checkpoint it, confirm no
`-wal`/`-shm` sidecars, run the database recovery drill so the source is in an
authenticated vault, create a fresh target, migrate it, and invoke:

```sh
source_digest="sha256:$(sha256sum data/manchester-arcana.db | awk '{print $1}')"
DATABASE_URL="$FRESH_TARGET_DATABASE_URL" \
  cargo run --locked -p manchester-dnd-server --features legacy-import \
  --bin legacy-import -- data/manchester-arcana.db "$source_digest"
```

Verify the report's IDs/counts/revisions/state hashes/timestamps, run a recovery
manifest, start the release build against the fresh target, load/resume the
campaign, and complete a new backup before switching configuration. Rollback is a
configuration switch to the untouched source/previous PostgreSQL database. Retain
the encrypted source and report through the rollback window; deletion requires a
separate explicit owner/operator decision.
