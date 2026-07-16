# Private-inspiration operator and incident runbook

This runbook is for the supported local, single-user deployment. Private
inspiration is off by default and remains unavailable unless all deployment,
campaign-safety, source-review, participant-verification, and consent gates are
current. Hosted use and public sharing remain unavailable.

The operator commands below accept only opaque IDs, closed policy values, epoch
times, and SHA-256 evidence digests. They never accept source Markdown, names,
contact details, filesystem paths, report prose, or consent prose.

## Roles and access boundary

- The source custodian controls the encrypted source volume and its recovery
  key. This person may install or remove raw Markdown.
- The consent reviewer verifies pseudonymous participant confirmations and
  records only evidence digests.
- Only the offline `inspiration-admin` process receives a decrypted source
  directory. It minimizes approved input to bounded neutral facts and writes an
  integrity-bound runtime projection to PostgreSQL.
- The web/game/image process reconstructs selectable prompts from that minimized
  projection on demand. It does not open `EVENT_PROMPT_DIR`, receive the source
  vault key, or retain raw Markdown in memory. Ordinary database backup and
  image-generation workers must not receive the source-vault key or decrypted
  mount. Real providers remain unapproved for private input; use only the
  deterministic fake provider for a private-inspiration exercise.
- The incident operator controls the durable global switch. The switch survives
  process restarts and is independent of `INSPIRATION_ENABLED`.

Do not combine the source-volume recovery key, database backup key, provider
credentials, and operator evidence in one file or secret-manager entry.

## Protected storage and backup

Store active plaintext only on a memory-backed or encrypted-at-rest volume
provided by the operating system/storage platform, such as LUKS, FileVault,
BitLocker, or an encrypted managed volume. The repository path is an empty
ignored mount point, not an approved place for plaintext sources.

After mounting the encrypted volume, enforce a private owner and a read-only
application view:

```sh
install -d -m 0700 /secure/manchester-arcana/private-sources
find /secure/manchester-arcana/private-sources -type d -exec chmod 0700 {} \;
find /secure/manchester-arcana/private-sources -type f -exec chmod 0600 {} \;
```

For a container, bind the decrypted directory read-only only into an ephemeral
`inspiration-admin` invocation. Do not mount it into the web or image worker:

```sh
--mount type=bind,src=/secure/manchester-arcana/private-sources,dst=/run/secrets/manchester-arcana-sources,readonly
```

Set `EVENT_PROMPT_DIR=/run/secrets/manchester-arcana-sources` for that admin
invocation. Unmount/delete the decrypted scratch directory immediately after
review/registration. Never copy the source root into a container image, web
runtime, CI artifact, support bundle, or static-site root.

The repository supplies an authenticated XChaCha20-Poly1305 vault for source
storage and immutable backups. Keep its random 256-bit key in a separate
mode-`0600` operator secret that is never configured on the web/game/image
process:

```sh
umask 077
install -d -m 0700 /secure/manchester-arcana/encrypted-backups
cargo run --locked -p manchester-dnd-server --bin source-vault -- \
  create-key /secure/offline/manchester-arcana-source-vault.key
cargo run --locked -p manchester-dnd-server --bin source-vault -- \
  seal /secure/manchester-arcana/private-sources \
  /secure/manchester-arcana/encrypted-backups/private-sources-$(date -u +%Y%m%dT%H%M%SZ).mavlt \
  /secure/offline/manchester-arcana-source-vault.key
```

The vault rejects symlinks, special/non-Markdown files, unsafe paths, excessive
counts/depth/bytes, weak key permissions, unknown schemas, tampering, and
existing restore destinations. It encrypts file paths and bodies; receipts
contain only ciphertext/source-tree digests, counts, and creation time.

Record `record_diagnostic_access` before restoring, decrypt into a new protected
scratch directory, run the reviewed loader, then remove the scratch directory:

```sh
cargo run --locked -p manchester-dnd-server --bin source-vault -- \
  restore BACKUP.mavlt /run/user/$UID/source-restore \
  /secure/offline/manchester-arcana-source-vault.key
```

Vault cleanup takes the current epoch, authenticates every candidate before
using its timestamp, and enforces the compiled 2,592,000-second (30-day)
boundary; a caller cannot shorten the retention by supplying a cutoff directly:

```sh
cargo run --locked -p manchester-dnd-server --bin source-vault -- \
  expire /secure/manchester-arcana/encrypted-backups \
  /secure/offline/manchester-arcana-source-vault.key "$(date +%s)"
```

Database backups follow the same 30-day policy under their independent key. An
opaque deletion tombstone expires after 35 days so deletion is carried through
every still-live backup. A restored database must replay all deletions whose
tombstones had not expired at the recovery point before it can serve traffic.

## Running the body-free admin tool

Build and run commands from a private operator shell. Put each request on an
encrypted volume or memory-backed directory with mode `0600`, and remove it after
recording the redacted response:

```sh
umask 077
cargo run --locked -p manchester-dnd-server --bin inspiration-admin -- /run/user/$UID/inspiration-command.json
```

The tool reads exactly one regular JSON file no larger than 64 KiB. Unknown
fields are rejected. It loads configuration afresh, applies embedded migrations,
reads the protected source tree only in this offline process, and returns JSON
only. Never paste secrets or raw source text into a command.

Every plaintext source, backup, quarantine, or generation-diagnostic access must
first use `record_diagnostic_access`. The durable row contains only an operator
ID, opaque subject, closed access/purpose/decision codes, evidence digest, and
timestamp. Exact retries reuse the receipt; an idempotency key cannot be reused
for different access:

```json
{
  "operation": "record_diagnostic_access",
  "idempotency_key": "restricted-access:restore:20260715",
  "campaign_session_id": "campaign:local-mvp",
  "operator_id": "operator:22222222222222222222222222222222",
  "access_kind": "source_backup",
  "purpose": "restore_drill",
  "subject_id": "source-vault:restore-drill",
  "evidence_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "decision": "allowed"
}
```

Use fresh idempotency keys for new decisions. Reuse the exact key and exact body
only to replay an uncertain command result.

## Installation and consent sequence

1. Keep the web/game process running without any decrypted source mount. Keep
   campaign inspiration disabled while preparing and reviewing files.
2. Verify the encrypted mount, ownership, backup recipient, and absence of source
   files from Git, images, CI artifacts, and browser assets.
3. Set `INSPIRATION_ENABLED=true` for the offline admin review process, record
   the plaintext access, then run
   `loaded_source_inventory`. The output contains only source IDs, exact digests,
   schema/counts, and enabled state.
4. Run `verify_participant` independently for every pseudonym. The verifier must
   not be the represented participant, and evidence must document a supported
   signed or two-channel confirmation without copying it into the command.
5. Run `register_loaded_source`. The tool derives participant IDs, sensitivities,
   the exact source digest, and the bounded neutral-fact runtime projection from
   the reviewed source rather than accepting them from the operator. Raw
   Markdown, title, source path, and review-only prose have no database column.
6. Run `review_loaded_source`. Approval records Q11 screening for that exact
   digest. A changed byte produces a different source ID/digest and must go
   through registration and review again.
7. Create campaign safety in two steps: first `configure_campaign` disabled at
   revision zero, then enable it at the returned revision with the same complete
   typed safety scope.
8. Run `grant_loaded_source_consent` separately for each source participant. A
   grant is exact to campaign, private audience, text media, high fictional
   distance, sensitivity scope, expiry, reviewer, and artifact policy.
9. Remove the decrypted mount. Only after every prerequisite is current, set the
   web process's deployment gate `INSPIRATION_ENABLED=true` without configuring
   or mounting a source root; then confirm `status` and play using the
   deterministic fake text provider. The server reloads only the minimized
   PostgreSQL projection. An
   approved source is still ineligible if any participant grant, safety scope,
   theme, media, expiry, cooldown, feature gate, pause, veto, or global control
   fails.

Minimal status request:

```json
{
  "operation": "status",
  "campaign_session_id": "campaign:local-mvp"
}
```

Global emergency disable request:

```json
{
  "operation": "set_global_control",
  "idempotency_key": "incident:20260715:disable",
  "expected_revision": 1,
  "generation_disabled": true,
  "operator_id": "operator:22222222222222222222222222222222",
  "evidence_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}
```

## Access, correction, revocation, and deletion

- `participant_export` returns the campaign settings, body-free metadata for
  sources involving only that requester, and only that participant's grants. It
  excludes source facts, operator IDs, confirmation/review evidence, and every
  other participant's consent record.
- Correct a source by pausing inspiration, removing the obsolete protected file,
  installing the corrected file, and registering/reviewing its new exact digest.
  Existing grants do not transfer. Revoke obsolete grants and collect fresh
  consent before resuming.
- `revoke_consent` immediately makes the grant ineligible, requests cancellation
  of pending derived work, and applies its stored delete/redact/minimal-audit
  policy to completed presentation artifacts without changing rolls or mechanics.
- Private-source image derivation is not an MVP capability: admin registration
  and consent are text-only, the derived-work boundary rejects an image from a
  text selection, and scene-image briefs have no private-input field. Revocation
  therefore has no private image body to chase; ordinary fictional scene images
  remain governed independently by their campaign/source-turn retention.
- In the game, pause, veil, source veto, category veto, all-inspiration disable,
  and privacy report controls require no reason text and act before later
  narration.

Participant deletion is deliberately ordered so no raw source can be forgotten:

1. Disable generation globally if the request may affect more than one campaign.
2. Remove every protected file whose metadata contains the participant pseudonym.
3. Ensure every newly sealed vault omits it, record the deletion evidence digest,
   and let already-immutable authenticated vaults expire at the enforced 30-day
   boundary; do not decrypt/rewrite them or silently extend retention.
4. Restart the admin command so it loads the current source root, then run
   `delete_participant_data`. The command refuses to proceed while any loaded
   source still contains the participant.
5. Confirm the result counts. In one transaction it revokes verification and all
   remaining grants, quarantines every associated registry source, cancels
   pending work, applies stored policy to completed artifacts, audits the
   transition, and creates a 35-day opaque tombstone.
6. Verify the participant cannot be re-verified while the tombstone is active.
7. At or after the returned expiry, run
   `purge_expired_deletion_tombstones`. The cutoff may not be in the future and
   exact retries are idempotent.
8. Before serving a database restored from backup, repeat steps 4–7 using the
   deletion register current at that recovery point.

Deletion request shape:

```json
{
  "operation": "delete_participant_data",
  "campaign_session_id": "campaign:local-mvp",
  "idempotency_key": "deletion:participant-1111:20260715",
  "participant_id": "participant:11111111111111111111111111111111",
  "operator_id": "operator:22222222222222222222222222222222",
  "deletion_evidence_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
}
```

## Incident response

For suspected source, consent, provider, credential, export, log, or artifact
exposure:

1. Run `set_global_control` with `generation_disabled=true`. This blocks new
   deterministic reservations, cancels pending private work, and redacts every
   completed private presentation in one durable transaction.
2. Set `INSPIRATION_ENABLED=false` for the next process start, stop provider
   traffic, and unmount the protected source volume. Do not delete evidence before
   the affected opaque IDs and digests are recorded.
3. Revoke provider/database credentials that may be exposed. Rotate source-volume
   and backup recipients separately. A credential rotation is not complete until
   the old credential is proven unusable.
4. Inspect only body-free privacy audits, command receipts, generation receipts,
   and artifact digests. Do not enable full prompt/body logging or copy raw files
   into tickets.
5. Apply participant revocation/deletion and artifact policy as required. Public
   share links do not exist in MVP, so there is no share token to preserve or
   invalidate.
6. Notify affected users according to deployment policy and applicable law. Use
   opaque references internally; send personal details only through the approved
   notification system.
7. Preserve the minimal incident record, verify backup expiry, and document the
   recovery decision. Re-enable the global switch only with a new evidence digest,
   repaired controls, a clean source inventory, and current participant consent.

## Evidence and prohibited data paths

Every exercise must scan the built WASM/JS/CSS, HTTP bodies, normal application
logs, metrics labels, exports, generated artifacts, and retained test evidence for
the exercise canaries. The scan must find no raw sentence, participant name,
contact value, source path, confirmation text, or provider credential.

Do not retain raw prompts or sources in tracing, analytics, crash reports, browser
storage, screenshots, support bundles, evaluation corpora, or fixture snapshots.
The only normal durable private-inspiration records are opaque IDs, exact digests,
closed policy values, minimized neutral facts, revisions, counts, timestamps,
deterministic selection/access receipts, and explicitly retained fictional
output subject to artifact policy.
