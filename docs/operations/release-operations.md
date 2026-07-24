# Private-MVP release and degraded-operation runbook

Status date: 2026-07-15. This runbook supplements the [database recovery](database-recovery.md),
[private-inspiration incident](private-inspiration-runbook.md), and [local server](slice-0-runbook.md)
runbooks. Commands apply only to the loopback private-evaluation profile.

## Safe operating rules

1. Never expose the current binary through a reverse proxy or non-loopback bind.
2. Treat `MONGODB_URI`, `MONGODB_SCHEMA_URI`, `DRAGONFLY_URL`, provider tokens, RNG/source/recovery keys, canonical
   exports, image files, and source mounts as secrets or private data.
3. Stop optional generation before invasive diagnosis. Deterministic play with
   authored fallback is the preferred degraded state.
4. Do not edit campaign, audit, receipt, job, consent, or artifact rows manually.
   Preserve the database and use tested application/recovery boundaries.
5. Record only UTC time, build revision, safe code/correlation ID, bounded counts,
   file digests, and operator decision. Never copy bodies into tickets or logs.

## Pre-release and startup

Run the pinned quality, provenance, secret, migration, release build, browser,
provider-degradation, role, and encrypted-restore gates listed in the release
evidence report. Start only when:

- `GET /health/live` and `GET /health/ready` return `204`;
- `database-ops` reports the expected migration version, no old backup/restore
  failure, no long transaction/lock wait, and bounded queue age;
- provider backends are `disabled` unless an approved deployment record exists;
- `INSPIRATION_ENABLED=false` unless every source/consent/safety prerequisite is
  current; and
- protected directories/keys have modes `0700`/`0600` and sit outside `public`
  and `target/site`.

## Provider outage or unsafe output

1. Confirm deterministic mechanics committed by reloading the campaign revision
   and stored roll history. Do not retry a different command or reroll.
2. Set the affected `TEXT_LLM_BACKEND` or `IMAGE_LLM_BACKEND` to `disabled` and
   restart. Provider configuration is startup-only by design.
3. For private-inspiration risk, also set the durable global generation switch,
   then set `INSPIRATION_ENABLED=false` and follow the incident runbook.
4. Verify authored narration/placeholder play completes and new outbound calls
   cease. Run the fake/disabled browser journey before re-enabling.
5. Rotate a suspected credential, prove the old credential fails at the provider,
   rescan logs/artifacts, and record only the credential identifier—not its value.

## Queue and lease recovery

1. Stop all workers using the same database before changing provider/config state.
2. Run `database-ops`; inspect bounded `generation_queue` totals, oldest runnable
   age, expired leases, and recent failure classes. Do not query prompt/output bodies.
3. Restart one release worker. Expired leases are closed body-free and reclaimed
   with a new attempt/lease token; queued work observes retry time and attempt cap.
4. Confirm queue depth/oldest age decreases and selected artifacts remain singular.
   Duplicate enqueue must replay the original job.
5. If the circuit remains open, keep the provider disabled until its cooldown and
   outage cause are resolved. Do not bypass it with direct provider calls.

## Artifact quarantine

1. Disable the image provider and stop the worker.
2. Record restricted diagnostic access before reading any quarantine file.
3. Compare only expected path, kind, byte count, dimensions, and SHA-256 digest with
   the database record. Never publish or attach unverified bytes.
4. Leave invalid output quarantined for bounded cleanup, or run the documented
   campaign deletion path. Do not move it into a static/public directory.
5. Re-enable only after spoofed MIME, oversized/pixel-bomb, transparent/unsafe,
   traversal, redirect/SSRF, authorization, and replacement tests pass.

## Pack or source quarantine

- **Pack:** stop startup, preserve the exact rejected directory/digest privately,
  and fix it as a new immutable version. Never alter a pack pinned by an active
  campaign. Restore the exact readable archived version or keep that campaign
  read-only/exportable.
- **Source:** set the durable global inspiration switch, remove the source from the
  decrypted mount, seal a new vault that omits it, register the new inventory, and
  apply correction/revocation/deletion. Grants never transfer to a changed digest.

## Disk full or storage read-only

1. Stop mutations and optional workers. Liveness may remain up; readiness or writes
   must fail rather than claim a save.
2. Preserve free blocks/inodes and mount status from the host without dumping private
   filenames. Check database, WAL, protected image/quarantine, and backup volumes
   separately.
3. Free only expired, policy-authorized artifacts/backups through bounded cleanup.
   Never delete MongoDB data/journal files, selected artifacts, RNG keys, or active vaults.
4. Take an encrypted logical backup when the database can still read, provision a
   larger private volume, restore into an isolated database, and compare the complete
   recovery manifest before cutover.
5. Re-run a commit/reload, selected-image delivery, and encrypted restore drill.

For a drill, use a disposable database and quota-limited temporary artifact volume;
never fill the development or production filesystem deliberately.

## Credential and key rotation

- **Database:** create/grant a replacement least-privilege login, deploy the new
  secret, drain old connections, revoke the old login, then run readiness and a
  revisioned commit/reload. Migration/backup/operator roles remain separate.
- **Provider:** disable backend, issue a replacement, restart with the replacement,
  revoke and test the old credential, then run fake/provider contract evidence.
- **RNG master key:** do not rotate in place for an active campaign. Existing seed
  references require the original protected material; use an explicit versioned
  key migration or retain read-only/export capability.
- **Source/recovery vault key:** create a new mode-`0600` key, authenticate/decrypt
  each live vault into protected scratch, reseal to a new path, verify exact digest,
  then expire/destroy the old key only after its retention set is empty.

## Migration failure, read-only mode, and release rollback

Follow the database runbook's migration/read-only procedure. Migrations are forward
only: never remove a migration row or run ad-hoc down SQL. If a release fails:

1. stop the new binary and preserve logs containing only safe metadata;
2. keep the database at its current migrated version;
3. start the previous binary only if its compatibility fixture explicitly accepts
   that version, otherwise use operator-enforced read-only export mode;
4. build the corrected forward release, restore into an isolated database when data
   risk exists, compare manifests, then cut forward; and
5. prove campaigns committed before the failed release remain loadable and campaigns
   committed by the new schema are not silently reinterpreted.

## Consent revocation and user deletion

Use the ordered procedures in the private-inspiration runbook. Immediately pause or
disable generation; remove protected raw sources first; revoke grants; cancel pending
work; apply delete/redact policy to derived presentation; create the 35-day opaque
tombstone; expire immutable backups at 30 days; and replay tombstones before serving
a restored database. Mechanical history stays intact and contains no source body.

## Release evidence and rollback decision

Retain body-free command results, test summaries, SBOM/provenance digests, image and
container digests, database recovery manifests, and the explicit go/no-go record.
Any critical unresolved data-loss, authority, consent, privacy, security, provenance,
or accessibility issue is a no-go. External branding/provider/manual-device holds
must be listed as blockers rather than converted into unsupported claims.

## Rewrite Part 1: Authenticated multi-user mode (2026-07-21)

### Migration scope
Migrations 0001-0031 must be applied in order. Key new migrations:
- 0027: Player character audit retention (FK ON DELETE SET NULL).
- 0028: Campaign memberships, invitations, character instances.
- 0029: Campaign membership theme_id.
- 0030: Campaign lobbies and turns (extends play_sessions, adds participants/turn-state/audit tables).
- 0031: Custom-action point ledger.

### Backfill behavior
- Existing `campaign_play_sessions.state = 'open'` rows are backfilled to `'waiting'`.
- `gm_account_id` is backfilled from `campaign_sessions.owner_account_id`.
- Legacy local-owner rows use `'account:local'` as a placeholder.
- Existing local campaign/hero/export fixtures remain loadable after all migrations.

### Hosted-mode gate
Hosted mode (`APP_ACCESS_MODE=hosted`) remains fail-closed in `validate_access_mode()`
until the evidence checklist in `docs/evidence/rewrite-part-1-auth-and-isolation.md` is
fully verified. Do not remove the gate without completing the two-account isolation matrix.

### Recovery
- Backup and restore accounts, sessions, memberships, character library, campaigns,
  lobby, turn state, audits, and point ledger.
- Verify canonical state hashes match before and after restore.
- Sessions may be excluded/revoked after disaster recovery; document expected re-login behavior.
