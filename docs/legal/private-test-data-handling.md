# Private-test data handling and reporting notice

Status date: 2026-07-15. This notice describes the loopback-only private MVP. It is
not a public-service privacy policy and does not authorize collection from the public.

## Deployment and data controller

The private-test operator who provided the build controls the local MongoDB system of
record, optional Dragonfly cache, protected files, provider accounts, backups, source
vault, and reporting channel. The application has no public share links,
behavioral analytics service, or built-in report submission endpoint.

## Data categories and purposes

- Campaign/hero state, typed player intent, deterministic rolls, immutable mechanical
  audits, and command receipts exist to save, explain, replay, export, and recover play.
- Generated narration/image metadata, selected outputs, usage/cost estimates, and
  policy fingerprints exist to present optional material, enforce limits, and support
  idempotent recovery.
- Optional real-life inspiration is processed only after verified scoped consent.
  Raw Markdown stays in separately protected operator storage; game state receives
  bounded minimized facts and opaque/digest policy records.
- Body-free operational counts, latency, health, queue/lease, fallback, budget, and
  denial signals exist to operate the private test. Campaign text, prompts, sources,
  identities, seeds, and generated bodies are prohibited as analytics labels.

## Retention

Active campaigns, created heroes, selected artifacts, and immutable mechanical audits
remain until owner deletion. Archives remain until restore/delete. Incomplete drafts
expire after 7 days plus a 30-day recovery/audit window. Failed generation metadata
expires after 7 days, restricted diagnostics after 14, unselected/superseded
presentation artifacts after 30, prepared exports after 24 hours, and encrypted
backups after 30 days. Opaque deletion tombstones may remain 35 days so deletion is
carried through all live backups. Exact policies are enforced by the application and
operator runbooks; legal hold is not an MVP feature.

## Providers and transfers

Text/image providers are disabled by default. The existence of an adapter is not
approval. Before enabling a real profile, the operator must review input/output rights,
retention/training, region/subprocessors, deletion, moderation, similarity, likeness,
takedown, credentials, and account-tier settings. Private inspiration must not be sent
to a real provider without an explicit deployment approval covering that purpose.

## Player and participant controls

The owner can inspect history, make readable/canonical private exports, archive,
restore, and delete the local campaign. Represented participants can request scoped
inventory/access, correction, consent review/export, immediate revocation, and
deletion without receiving another person's source body or consent record. In-game
pause, veil, source/category veto, disable-all, and privacy report controls require no
reason and do not rewrite committed mechanics.

## Reporting

Stop the affected feature and contact the operator through the out-of-band channel
used for the private-test invitation. If it is unavailable, stop the local server and
retain encrypted evidence until a secure route is supplied. Report only build revision,
approximate UTC time, safe error code/correlation ID, affected feature, and impact.
Never send campaign/source prose, identities, screenshots with private content,
database dumps, exports, keys, credentials, or provider bodies.

Security/privacy incidents follow the [release operations](../operations/release-operations.md)
and [private-inspiration incident](../operations/private-inspiration-runbook.md)
runbooks. Public distribution remains blocked pending explicit licensing and external
working-name/domain/trademark clearance.
