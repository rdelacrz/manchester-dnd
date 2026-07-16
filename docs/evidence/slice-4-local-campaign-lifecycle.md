# Slice 4 local campaign lifecycle evidence

Status: implemented across PostgreSQL, the local-only application boundary, and the browser campaign library. This is evidence for the explicit loopback `local-single-user` profile only. It is not evidence of hosted identity or cross-user security.

## Delivered boundary

- Every lifecycle query is scoped by both an opaque owner key and campaign ID. The current application supplies the fixed `local-owner`/`local-campaign` pair; it never accepts an owner key from the browser.
- Campaign lifecycle has an independent monotonic revision. Play start/end, archive, restore-from-archive, delete, and restore-from-export use exact revision checks and retained idempotency receipts without changing mechanical turn revisions.
- Play sittings are explicit durable rows with one-open-session enforcement, starting/ending campaign revisions, timestamps, and close reasons.
- The hydrated campaign library provides list/create/resume, start/end sitting, archive/restore, paginated history, canonical/readable export, prepared permanent delete, and canonical restore controls. Player-facing lifecycle/history labels use revisions and turn numbers rather than displaying unlabeled database timestamps.
- Turn history is ordered, cursor-paginated, bounded to 100 rows, and parsed/validated from immutable stored audits. Dice and rule facts are never reconstructed or rerolled.
- Archive has no scheduled expiry. Permanent delete requires archive state, no open play sitting, an explicit confirmation, and a short-lived server-prepared canonical export bound to both current revisions.
- Delete cascades every database row owned by the campaign, including the baseline character FK that previously used `SET NULL`. A 35-day tombstone keeps only opaque IDs, revisions, a SHA-256 export digest, and timestamps. Exact delete replay is served from a separately retained bounded receipt.
- Canonical JSON export uses a strict versioned schema and deterministic object-key ordering. Restore rejects unknown fields, unknown schema versions, invalid documents, invalid/unsealed provenance for a playable campaign, owner mismatches, and non-canonical input.
- Restore preserves campaign/character/hero documents, mechanical revisions, ordered turn and hero audits, command receipts, reward claims, validated pins, play-session evidence, selected text presentations, selected generated-asset references, and provenance digests. Campaign-lifetime narration aliases are exported without a body, including aliases whose superseded body has expired. Typed-intent recovery receipts include the player-text SHA-256 digest, original revisions, closed validated `EncounterIntent`, and bounded interpretation evidence metadata—never the raw player text. It never imports an old open play sitting as open; it closes it with `restore_import` and writes a new immutable lifecycle audit/revision.
- The player-readable private export contains the character sheet, campaign summary, committed stored audit facts, dice/rule facts, selected presentations/assets, exact pins/provenance, and attribution.
- Owner-only durable recaps are deterministic Markdown artifacts derived from committed audits. They carry campaign revision, ordered source range/count/digest, template ID, body digest, idempotency receipt, and no share token. Consent-scoped private-source wording is omitted unless recap consent is separately available. Recaps reload, export/restore, and cascade-delete with the campaign.
- Canonical import has a dedicated `POST /api/local/campaign/restore` boundary. It requires the exact versioned media type, a same-origin loopback `Origin`/`Host` pair, an explicit restore header, and a valid idempotency key. Declared and streamed/chunked bodies are capped at 2 MiB; the ordinary server-function ceiling remains 64 KiB.
- PostgreSQL connections use a bounded pool plus acquire, statement, lock, and idle-transaction timeouts. The ordinary transaction level is read-committed; canonical export is repeatable-read/read-only. Embedded migrations can run in a separate operator process.

## Deliberate exclusions

The export schema has no fields or queries for credentials, environment values, raw prompt bodies, raw player intent, raw private inspiration sources, generation request/response bodies, failed/unselected generation attempts, or other participants' consent. Selected presentation rows carry only the bounded safe player-visible body and copied provenance digests. Superseded/expired presentation aliases carry no body. Selected media carry protected relative keys and allowlisted metadata, not public URLs.

## Automated evidence

Run:

```sh
cargo test -p manchester-dnd-server repository::lifecycle::tests -- --nocapture
cargo test -p manchester-dnd-web
cargo check -p manchester-dnd-app --features ssr
cargo check -p manchester-dnd-app --features hydrate --target wasm32-unknown-unknown
scripts/check-postgres-role-policy.sh
scripts/run-database-recovery-drill.sh
scripts/run-legacy-import-drill.sh
```

The SQLx suite proves:

1. owner-scoped list/read behavior, exact replay, stale revision rejection, explicit play boundaries, archive/restore, and the exact migrated `ON DELETE CASCADE` character FK;
2. forged/unprepared delete rejection, full database cascade, receipt replay after deletion, tombstone field minimization, the 35-day boundary, and before/after-expiry cleanup behavior;
3. an export larger than 64 KiB round-trips eight ordered audits including a stored d20 roll, validated content/prompt/schema pins, selected text provenance/client idempotency, a retained narration replay, an expired-body replay, and a committed typed-intent replay without raw player text;
4. an exported open play sitting restores closed with `restore_import`, with a new import audit and no implicit resumed sitting;
5. unknown export schemas and an unsealed playable export fail closed;
6. a private recap is owner-scoped/idempotent, survives canonical restore with the exact body/source digest, reloads after restart, and disappears on cascade delete;
7. the chunked recovery vault rejects wrong keys, tampering, partial output, symlinks, and permissive key modes, round-trips across a chunk boundary, and expires at the exact 30-day boundary.

The web binary suite separately proves exact restore media/custom/origin headers, forged-origin rejection, the ordinary-versus-restore declared limits, and an actual multi-chunk body exceeding 2 MiB. `tests/browser/slice4-lifecycle.spec.ts` exercises the local UI through a start/end sitting, history/export, archive, prepared deletion, canonical route restore, and explicit unarchive.

## Play-session enforcement residual

Play sessions are currently explicit durable bookkeeping boundaries and lifecycle interlocks: only one can be open, and archive/delete cannot proceed while one is open. Existing mechanical application commands still commit without requiring an open play-session row. The current evidence therefore does not claim that every turn is grouped under an open sitting. Making that a hard application precondition, updating browser affordances, and migrating the direct command/test fixtures is separate remaining work.

## Identity and authorization residuals

- Hosted mode remains fail-closed. No account records, login, browser authentication session, logout/revocation, cookie, CSRF-token, throttling, cross-user authorization matrix, or hosted object-level authorization is claimed.
- Owner-scoped repository signatures and non-enumerating application mapping are readiness seams, not proof of cross-user isolation. Hosted access must not be enabled until every route, job, export, and artifact delivery path uses authenticated membership and the full authorization matrix passes.
- The fixed local mode still trusts the loopback deployment profile. It must remain separately configured and must never be inferred from an arbitrary hostname in hosted mode.

## Backup, asset, and operations evidence/residuals

- The recovery drill created a custom-format logical dump, bundled the exact RNG key, protected artifact tree, deterministic manifests, checksums, and retained legacy SQLite file, sealed it with chunked XChaCha20-Poly1305, opened it, restored into a uniquely named database, matched source/restored state hashes, exercised exact 30-day expiry, recorded operational/disk evidence, and dropped the isolated database. It never restored over the source.
- Fixed `NOLOGIN` migration/app/backup/operator groups pass attribute and privilege assertions. The app cannot create databases/roles/schema or update operator recovery status; backup is read-only; the operator cannot read campaign tables. Local Compose remains the documented combined-administrator development exception.
- The body-free operations snapshot reports migration/database/index/WAL size, connections/waits/long transactions, lock waits/deadlocks/I/O time, analyze/dead-tuple state, optional replication lag, queue/lease age, and last backup/restore result. Disk capacity is captured by the drill. Alert/SLO thresholds remain intentionally unset until representative private measurements exist.
- Logical recovery meets the manual private-MVP RPO. Physical/base backup plus WAL/PITR is a documented profile trigger rather than a current requirement; it becomes mandatory before a shorter time-based RPO or hosted/large-database profile is enabled.
- The feature-gated legacy importer opened the retained SQLite v1/v2 database immutable/read-only, required its precomputed SHA-256, imported one campaign/one character atomically into an isolated fully migrated PostgreSQL database, matched counts/revisions/links/timestamps/state hashes, replayed with zero inserts, rejected a wrong digest before publication, and dropped the target. The source digest remained `sha256:587ef01eb17b1fcd3b4309e718d3e01cd12f17325874406e2f400980bc25bbc1`. The source remains retained; a production cutover/deletion was neither required nor performed.
- The recovery manifest verifies every selected scene-image original/web/thumbnail path and SHA-256 when present, and its path/digest boundary has an automated non-empty artifact test. The recorded live drill had no selected artifact in its sampled campaign; a non-empty database-level artifact restore rehearsal remains release evidence to collect after the consolidated internal image journey.
- Permanent database deletion cascades artifact reference rows. Protected scene-image cleanup exists for normal artifact retention, but coordinating owner campaign deletion with immediate filesystem deletion still needs a dedicated deletion hook/drill.
- Lifecycle receipt cleanup is 30 days; delete preparations are one hour; deletion tombstones are 35 days; encrypted backups expire at 30 days.
- Public/shareable recap remains a separately authorized post-MVP projection and is not produced by these private exports.
