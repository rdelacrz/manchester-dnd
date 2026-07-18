# Slice 5 consent and privacy evidence

Date: 2026-07-15

This evidence covers the implemented local-MVP consent boundary, encrypted
source-vault/backup drill, and complete revocation/deletion exercise. It does not
approve a real provider or hosted deployment: release threat-model review,
provider-policy approval, and represented-participant usability testing remain
release gates. The deployment flag still defaults off, and only the
deterministic fake provider is permitted to consume minimized
private-inspiration facts.

## Implemented boundary

- The web/game/image process never opens the protected source root, regardless
  of the deployment gate. Only the offline admin review process uses the
  canonical bounded loader, which rejects traversal, symlinks, special files,
  invalid UTF-8, active resources, excessive depth/count/bytes, malformed
  metadata, likely contact/identity material, direct quotations, prompt
  injection, and Q11 prohibited categories.
- Approved Markdown becomes one to four bounded neutral facts plus typed policy.
  The admin persists an integrity-bound runtime projection. Raw bytes, path,
  filename, title, source-authored transformation prose, names, and contact
  values do not enter PostgreSQL or the model boundary. Live selection reloads
  only this minimized projection, so the web/image worker receives neither a
  decrypted mount nor its key.
- PostgreSQL records exact source digest/version, opaque source and participant
  IDs, provenance, themes, media/audience, category/sensitivity scope, review,
  participant verification, consent expiry, transformation, and artifact policy.
- Deployment, durable global, campaign enable/pause, complete safety, exact
  participant consent, source review, media/audience, expiry, campaign theme,
  veto, cooldown, and safe-trigger gates all fail closed before weighting.
- A selected source uses server-owned `chacha20-v1` authority. Selection, draw,
  eligible-set digest, source digest/version, cursor interval, cooldown, and
  no-selection reason commit atomically. Exact retries replay; an ineligible set
  advances no cursor. A fixed-seed 8,192-draw distribution test gives a
  million-weight ineligible source exactly zero selections while the eligible
  1:3 pair remains within the predeclared 72–78% band.
- Only minimized facts and the compiled high-fiction-distance policy enter the
  typed narration request. Identifier, markup, quotation/similarity, policy,
  consent, and mechanical-fidelity checks reject unsafe output and select an
  unrelated authored fallback without probing the source again.
- The presentation projection exposes only that consented minimized material was
  used. It does not expose source or participant IDs. Public share namespaces
  return `404 {"code":"public_sharing_unavailable"}`.

## Immediate controls and data rights

The player UI always renders pause, resume, all-inspiration disable, veil,
source veto, category veto, and privacy report controls. Passage interventions
carry only campaign, presentation, command, and closed action IDs. They request
no explanation and never rewrite rolls, HP, XP, encounter outcome, or campaign
history.

The body-free `inspiration-admin` binary supplies source inventory,
participant verification, source registration/review, campaign safety,
consent grant/review/revocation, participant-scoped export, participant deletion,
the global switch, and tombstone expiry. Its tagged JSON enum denies unknown
fields and has a 64 KiB regular-file limit; tests explicitly reject a
`raw_markdown` field.

Participant deletion requires the protected file to be absent from a freshly
loaded source root. One PostgreSQL transaction then:

- revokes the pseudonymous verification and every active/expired grant;
- quarantines every registry source involving the participant;
- requests cancellation of every pending derived work item;
- applies each completed artifact's stored delete/redact policy;
- writes body-free command and privacy audits; and
- creates an opaque deletion tombstone expiring after exactly 3,024,000 seconds
  (35 days).

Re-verification is denied while the tombstone exists. An idempotent global purge
accepts only a non-future cutoff, audits each expiry with opaque IDs, and permits
fresh verification only after the retention boundary.

Private-source images are structurally unavailable: the admin registers/grants
text media only, an image derived-work request from a text selection is rejected,
and the closed scene-image brief has no private-input field. Revocation therefore
cannot leave a private image artifact behind. The same integration exercise
proves pending text cancellation, DeleteDerived deletion with body-free receipt
retention, RedactDerived replacement, participant export, live-data deletion,
and the unchanged mechanical audit.

Restricted plaintext, backup, image-quarantine, and generation-diagnostic access
uses an idempotent operator command. Its dedicated PostgreSQL row contains only
an operator ID, opaque subject, closed access/purpose/decision codes, evidence
digest, and timestamp. The exercise records and exactly replays a restore-drill
access receipt.

## Live browser acceptance

`tests/browser/slice5-live-inspiration.spec.ts` runs against an isolated database,
the deterministic fake provider, and one synthetic source containing two raw
canaries. Through the real UI and operator boundary it:

1. creates and reloads a rules-valid hero;
2. records complete campaign safety, verified participant mapping, source review,
   and exact consent;
3. confirms the campaign status is enabled behind consent/safety gates;
4. plays deterministic combat while all mid-combat trigger windows remain closed;
5. reserves the source only for the victory safe boundary;
6. displays the high-distance private-inspiration provenance without IDs;
7. vetoes the passage and immediately replaces it with unrelated engine-authored
   narration;
8. proves the participant export contains no raw canary;
9. proves the public-share namespace is an explicit 404; and
10. scans captured text/JSON/JavaScript/CSS responses for both raw canaries, the
    participant pseudonym, and the selected source ID.

Result:

```text
npm run test:browser:slice5
1 passed (7.6s)
```

The release artifact scan covers generated WASM, JavaScript, CSS, any source maps,
the release server binary, and retained typed-GM evaluation corpora:

```text
PRIVATE_INSPIRATION_LOG_PATH=target/playwright/slice5-server.log \
  scripts/check-private-inspiration-boundary.sh
private-inspiration boundary scan: release, source-map, evaluation, and configured log artifacts are clean
```

The canary set includes both raw-source sentences, the participant pseudonym,
and the exact derived source ID. The browser captures text/JSON/JavaScript/CSS
responses; the release scan covers WASM, JavaScript, CSS, source maps, server
binary, evaluation corpora, and the real structured server log. The
PostgreSQL integration separately serializes the bounded operational metrics
snapshot and proves it contains no minimized fact, participant ID, or source ID.
There is no behavioral analytics or support-bundle implementation in the MVP.

## PostgreSQL lifecycle acceptance

The real-database integration test covers settings replay, pause/resume,
participant and theme exclusions, deterministic zero-probability/no-cursor
results, exact selection replay, text work completion, consent revocation,
pending cancellation, DeleteDerived deletion with body-free receipt retention,
RedactDerived replacement, global quarantine and recovery, participant and owner
vetoes, privacy report pause, scoped export, participant deletion, 35-day purge,
and re-verification gating.

```text
cargo test --locked -p manchester-dnd-server \
  repository::inspiration::tests::consent_selection_revocation_veto_and_export_are_atomic_and_redacted \
  -- --exact --nocapture
1 passed; 0 failed

cargo test --locked -p manchester-dnd-server --bin inspiration-admin
2 passed; 0 failed

cargo test --locked -p manchester-dnd-server source_vault::tests -- --nocapture
2 passed; 0 failed

scripts/run-private-source-vault-drill.sh
source vault drill: authenticated encryption, exact restore, pre-cutoff retention, and at-cutoff expiry passed

cargo test --locked -p manchester-dnd-server \
  events::tests::weighted_selection_distribution_excludes_every_ineligible_source \
  -- --exact --nocapture
1 passed; 0 failed

cargo test --locked -p manchester-dnd-app --features ssr --bin manchester-dnd-web
6 passed; 0 failed

cargo clippy --locked --workspace --all-targets -- -D warnings
passed
```

Twenty-two ordered migrations pass static validation. Migrations 0013–0020 establish
the consent registry, immediate controls, presentation/work binding, typed safety,
theme scope, global switch, participant deletion, and retention tombstones.
Migration 0022 adds the minimized runtime projection and restricted-access audit.

The source vault uses XChaCha20-Poly1305 with a random 256-bit mode-`0600`
operator key, random nonce, authenticated version/timestamp header, bounded
strict payload, encrypted paths/bodies, mode-`0700`/`0600` atomic restoration,
tamper/wrong-key rejection, and zeroization of decoded bodies where practical.
Cleanup authenticates each candidate before its timestamp is trusted and
computes the exact 2,592,000-second boundary internally. The live drill proves a
raw canary is absent from ciphertext, restore is byte-exact, a vault survives one
second before the boundary, and is deleted exactly at it.

## Operations and remaining release gates

The [private-inspiration operator and incident
runbook](../operations/private-inspiration-runbook.md) defines encrypted-volume
ownership, read-only mounting, independently encrypted source backups, role/key
separation, installation/review, correction, export, revocation, deletion,
30-day backup expiry, 35-day tombstone propagation, global shutdown, credential
rotation, artifact quarantine, notification, recovery, and prohibited log/support
paths.

Before changing the deployment default or using real personal material, finish
the release threat model, approve any real provider's
retention/training/region/deletion/moderation terms, rehearse the deployment's
independently encrypted database backup restore, and complete consent/usability
testing with represented participants. Scene images remain fictional and
private-source-free under the Slice 6 boundary.
