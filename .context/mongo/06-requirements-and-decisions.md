# Requirements Traceability, Decisions, and Release Gates

## Status legend

- **Designed** — target model/workflow is specified in this plan.
- **Decision required** — implementation depends on a product/operations policy not supplied in the brief.
- **Future tier** — schema supports it but first-release content/mechanics scope must be chosen.

## User requirement traceability

### Authentication

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| Support access tokens and associated sessions; operator generates tokens via CLI | `signup_access_tokens` stores only token digest/state/expiry/reservation; one-use token creates `signup_sessions` in a transaction; `mongo-admin signup-token generate/revoke/list` | **Designed.** Raw token printed once; reserve/consume/revoke/expiry/concurrency tests |
| Sign up to create actual account with session token expiring after one day | Access token creates 24-hour `signup_sessions`; completion creates account and normal login session atomically | **Designed.** Server rejects after timestamp even before TTL deletion |
| Authenticate with email, username, password | Unique normalized username; email keyed lookup HMAC; Argon2id password PHC; generic failures/throttling | **Designed.** Duplicate/case normalization, timing/dummy verify, throttle tests |
| Email encrypted “DB side” | Trusted app/driver encrypts before MongoDB; randomized AEAD ciphertext + key ID; unique HMAC lookup; DB never receives plaintext | **Designed, key decision required.** KMS/secret provider and rotation owner must be selected |
| Password hashed and salted with advanced scheme | Argon2id PHC with random salt and benchmarked parameters | **Designed.** Benchmark/rehash/concurrency/redaction tests |
| Admin and user accounts; only operator initially admin; admin via CLI | `accounts.role=admin|user`; web signup hard-coded to user; CLI-only admin creation/promotion with audit | **Designed.** Self-promotion/replay/admin audit tests |
| All other functionality authentication-protected | `ServerContext`/router middleware resolves principal; repository methods accept principal-derived account/role | **Designed.** Anonymous route matrix plus direct repository isolation tests |

### Campaigns

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| Create campaign with owner user ID | `campaigns.owner_account_id`; owner embedded as active GM member in same transaction | **Designed** |
| List campaigns | Query owner/member multikey indexes; return authorized projections only | **Designed.** Account A/B/admin list tests |
| Delete campaign, not if currently active | Reject while any play session is `waiting|active` or encounter active; explicit deletion manifest/tombstone/receipt | **Designed.** “Active” definition below must be accepted |
| AI campaign image generation | `generation_jobs` + external bytes + `generated_assets`; authorized campaign pointer publication | **Designed.** Provider/object failure and guessed-ID tests |
| Snapshot base character/enemy metadata during active campaign | Character/enemy instance source snapshots with ID/revision/schema/digest/payload; encounter-start combat snapshots; sealed campaign rules snapshot | **Designed.** Mutation invariance tests |
| Owner can CRUD own; admin any | All filters bind owner/member/admin policy; inaccessible IDs safe-not-found; override audited | **Designed.** Two-account matrix |

### Characters

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| Create/update/list/delete account-owned character | Level-less `player_characters` plus expiring drafts, revisions, owner indexes | **Designed** |
| Different subclasses and attributes/skills | Versioned build blueprint and content IDs; campaign instance derives active features at level; future class progression supports multiclass | **Designed; content breadth future tier** |
| Cannot delete if in active campaign | Reject when any active `campaign_character_instances` references source character | **Designed** |
| AI image generation | Character-scoped job/asset publication, external bytes, owner authorization | **Designed** |
| Level up per campaign | Progression is only in `campaign_character_instances`; explicit level choices/history and derived sheet | **Designed.** XP/milestone policy decision required |
| Campaign-unique level, money, BDE, etc. | Instance runtime owns progression, currency/inventory, HP/resources/conditions/BDE balance; BDE ledger separate | **Designed** |
| BDE earned/spent for special custom AI actions | Validated interpretation, deterministic effects, conditional balance decrement + ledger/event/receipt in one transaction; failure costs zero | **Designed; economy values/earning policy required** |
| Owner CRUD; admin any | Level-less library owner scope; runtime follows campaign membership/turn rules; admin override audited | **Designed** |

### Enemy instances

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| Create custom enemies with descriptions, attributes, levels, abilities | Complete versioned `enemy_templates` stat blocks + campaign snapshots/runtime instances | **Designed; ruleset content validator work required** |
| Update/list enemies | New immutable template revision marked current; old snapshots preserved | **Designed** |
| “campaign owner only” and “only admin edit for now” | Admin edits global templates; campaign owner instantiates/generates and controls owned-campaign enemy runtime; future owner templates can be separately scoped/reviewed | **Decision interpretation proposed.** Confirm below |

### Battles

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| Generate enemies | Typed generation proposal constrained by allowed mechanics and campaign rules; deterministic validation; publish as draft/template/instance | **Designed** |
| Store turn metadata | `encounters` current state + append-only `turn_events` with intent, roll facts, effects, revisions, result, presentations | **Designed.** Replay/restart/concurrency tests |

### Events

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| Create/update events | Versioned `event_templates`; update creates revision; campaign selection snapshots template | **Designed** |
| Admin-only editing initially | Admin role enforced on template mutations; campaign owner may trigger/resolve eligible instances, not alter global template | **Designed** |

### AI game master coordination

| Requirement | Planned design | Status / acceptance evidence |
|---|---|---|
| AI model selected by environment | Provider/model/credential from deployment config; DB stores only non-secret fingerprint and per-attempt provenance | **Designed** |
| Battle and exploration modes | `play_sessions.mode`, typed phase/turn state; `encounters` for battle; social/events as exploration subtypes | **Designed** |
| Turn-based GM action/dialogue and several choices | Typed GM proposal based on current revision/event sequence/legal IDs; selected presentation separate from mechanics | **Designed** |
| Players select predetermined action | Opaque action ID bound to legal-action-set digest; client sends no mechanics | **Designed** |
| Custom prompt by spending BDE | Constrained typed interpretation; applicability/safety/rules validation; atomic mechanic commit and spend only on success | **Designed; text retention/economy decisions required** |
| Random applicable results | Server-owned deterministic RNG/roll facts and typed effect set; AI narrates committed outcome, does not invent it | **Designed** |

## Project-context functionality retained beyond the brief

The brief is explicitly non-exhaustive. The plan also preserves or improves:

- campaign invitations, membership, lobbies, readiness, start policies, absent-player AI handoff;
- rules/content/prompt/policy pins and immutable encounter snapshots;
- optimistic revisions, exact idempotency, deterministic rolls, reward claims;
- short/long rests, hit dice, death saves, conditions, resources, spells, inventory, movement/action economy;
- generation jobs, leases, retries, budgets, moderation/quarantine, versioned presentations;
- campaign private recaps;
- private-inspiration participant verification, consent, safety configuration, veto, kill switch, deterministic selection/cooldown, deletion;
- deletion preparation/export, tombstones, audits, recovery state, object integrity, backup/restore;
- hosted two-account isolation and fail-closed startup.

## Required decisions before implementation

### D1 — Rules edition and migration sequencing

**Question:** retain current mechanics while changing persistence, or simultaneously rebuild to SRD v5.2.1?

**Recommendation:** persistence first under explicit existing ruleset ID; SRD v5.2.1 as a separately tested content/rules pack. This isolates database risk from rules-engine risk and still permits greenfield storage.

### D2 — Sign-up and account-session lifetime

**Proposed interpretation:** one-use access token creates a 24-hour sign-up session. Successful creation issues a normal account session governed by separate idle/absolute limits.

Decide normal idle/absolute duration, remember-me behavior, and whether first account session is also capped at one day.

### D3 — Email key custody

Choose:

- managed KMS/secret manager;
- MongoDB in-use encryption with supported KMS and separate uniqueness HMAC;
- root-readable local key files for private deployment, with offline recovery copy.

Define key owner, rotation schedule, disaster recovery, and startup behavior when a key version is unavailable. **Hosted mode must fail closed.**

### D4 — Campaign activity and deletion

**Recommendation:** campaign `open|archived`; “currently active” means a waiting/active play session or active encounter. Archived campaign is readable and cannot start a session until reopened; deletion remains separate.

Decide whether campaign deletion requires an export/confirmation and retention lengths for tombstones, audits, recaps, and artifacts.

### D5 — Enemy authoring permission ambiguity

**Recommendation:** admin-only global template editing; campaign owner can instantiate/generate and manage campaign runtime. Future owner-authored templates use owner scope + draft/review, not global edit privilege.

### D6 — Party and control policy

Decide:

- max party size (recommended product default 4–6, hard safety ceiling lower than current 64);
- one character per account per campaign (recommended yes);
- invitations by account, protected email, or random join code;
- `wait_for_all` versus `start_with_ai_substitutes` default;
- group-choice voting/tie-break and timeouts;
- whether AI can control absent players and which choices remain prohibited.

### D7 — Progression/content scope

Decide:

- XP or milestone default;
- starting level and max initial supported level;
- classes/subclasses/species/backgrounds in first content pack;
- ability score, HP, feat, multiclass, encumbrance, resurrection, and respec policies;
- which edition's terminology/content is exposed.

Schema supports both XP and milestone; one campaign pins exactly one policy.

### D8 — BDE economy

Decide:

- starting and maximum balance;
- fixed/variable custom action cost;
- grant triggers/amounts and whether individual/party-wide;
- applicable action envelope;
- refund/cancellation semantics;
- death/retirement carry behavior;
- admin grant/refund limits and justification.

**Recommended invariant:** integer and campaign-character scoped; no transfer; AI substitute cannot spend; no charge unless mechanics commit; every delta ledgered.

### D9 — Player custom text and narration retention

Decide whether full custom prompts and generated dialogue are required in campaign history, who may see them, retention, export/deletion, and encryption. Generic audit logs never contain them.

### D10 — Generation and object storage

Choose text/image providers, model allowlist, moderation, budgets, object storage, signed/authorized delivery mechanism, asset retention, and acceptable authored fallback. Provider/model config is environment/secret backed; changing it changes fingerprint, not past content.

### D11 — Private inspiration launch scope

Current code/schema has extensive privacy machinery. Decide whether the feature launches with Mongo rewrite or remains disabled behind kill switch. Even if deferred, do not weaken/remove its source-vault, consent, safety, and deletion boundaries.

### D12 — Production topology and operations

Choose managed Atlas versus self-managed replica set, regions, TLS/auth mechanism, backup frequency/retention, restore objectives, monitoring, KMS integration, and object-store consistency policy. A standalone `mongod` is not supported because authoritative commands use transactions.

### D13 — DragonflyDB cache layer (resolved)

**Decision:** DragonflyDB will be deployed via Coolify on the Orange Pi alongside MongoDB as an optional caching/pub-sub layer. It is not required for launch; the application is fully correct without it. Integration is phased: session cache and throttle after Phase 3, pub/sub after Phase 5, optional generation queue only if polling becomes a bottleneck. See [07-dragonflydb-integration.md](07-dragonflydb-integration.md).

## Security invariants that are not product options

1. No plaintext password storage or reversible password encryption.
2. No raw admission/session/CSRF tokens in MongoDB or logs.
3. No email plaintext sent to MongoDB under the selected field-encryption design.
4. No web/API route can create or promote an admin.
5. No browser-supplied owner ID/role establishes authorization.
6. No LLM/provider output directly mutates authoritative mechanics/economy.
7. No BDE charge on rejected/stale/failed custom action and no double charge on replay.
8. No TTL-dependent authorization.
9. No external provider/object-store call inside a MongoDB transaction callback.
10. No active campaign deletion or implicit cross-collection cascade assumptions.
11. No generated/private asset served by guessed asset ID without parent authorization.
12. No hosted launch before two-account isolation, crypto, transaction, backup/restore, and protected-media gates pass.
13. No raw tokens, passwords, email, encryption keys, or PII in DragonflyDB. Session cache entries contain only opaque IDs, timestamps, and `password_role_version`. DragonflyDB keys are always digests, never raw tokens.
14. No authorization decision based solely on DragonflyDB cache presence. Every cache hit still validates expiry and `password_role_version`; cache miss always falls through to MongoDB.

## Acceptance scenario matrix

### Auth and account

- Operator generates a one-use token, sees raw value once, and MongoDB contains only digest.
- Recipient reserves it and gets a 24-hour secure sign-up session.
- Concurrent second reservation fails; expiry/revocation/consumption fails even when document remains pending TTL.
- Account creation encrypts email, enforces unique email/username, hashes password with current Argon2id policy, consumes token, and issues session atomically.
- Login works with username and email; generic errors reveal neither account existence nor email.
- User cannot elevate role; CLI admin can perform audited cross-owner management.

### Ownership and snapshots

- Account A cannot list/read/mutate B's character/campaign/asset through API or direct repository call.
- One source character creates independent instances in campaigns X/Y.
- Source character/template update changes no active instance or encounter snapshot.
- Active instance blocks source-character deletion.
- Active play/encounter blocks campaign deletion; closed campaign deletion follows manifest and exact replay.

### Turns and BDE

- Two concurrent actions against one expected revision yield one commit and one conflict/replay.
- Same receipt returns same roll/result; changed payload conflicts.
- Out-of-turn player, stale action ID, and wrong legal-set digest fail without state change.
- Accepted custom action spends once and records ledger/event/audit/receipt; failure costs zero.
- Battle completion synchronizes HP/resources and awards XP/loot/BDE exactly once.

### GM/generation/assets

- GM timeout/rate limit/malformed/unsafe/contradictory/stale output falls back or rejects without unauthorized mechanics.
- Worker crash leaves reclaimable lease; duplicate worker cannot publish twice.
- Image bytes fail size/type/dimension/moderation before publication and are quarantined/cleaned.
- Account B cannot fetch A's character image or another campaign's scene asset via ID.

### Events/privacy

- Admin creates new event/enemy template revision; prior campaign snapshot remains unchanged.
- Event eligible set/selection/cooldown replays deterministically.
- Lines/veils/consent/veto/kill switch exclude unsafe/private source use even under races.
- No raw private source body/filename/identifier appears in ordinary DB, provider prompt, audit, logs, or public DTO.

### Operations

- Clean DB schema apply/verify succeeds and repeated apply is no-op.
- Validator/index drift fails hosted startup.
- Primary stepdown/transient transaction/unknown commit does not duplicate effects.
- Restore with MongoDB + object storage + content/prompt pins + keys reconstructs current actor, revisions, HP/resources, BDE, and exact replay.
- Missing key/artifact/content digest fails closed and is diagnosable without leaking secrets.

## Release gates

### Gate A — Persistence foundation

- Replica set healthy; schema/index bundle verified; transaction/idempotency fault tests pass.

### Gate B — Identity

- Admission/signup/login/session/Argon2id/email crypto/admin CLI and anonymous protection pass.

### Gate C — Tenant isolation

- Complete two-account plus admin matrix passes at HTTP/server-function/repository/asset levels.

### Gate D — Game authority

- Snapshot, revision, turn, RNG, encounter completion, reward, and BDE concurrency tests pass.

### Gate E — AI and media

- Typed GM failure/staleness, generation leases/budgets, moderation, external bytes, and access control pass.

### Gate F — Privacy and lifecycle

- Event/private-inspiration safety, deletion/export/tombstone, account/campaign erasure and audit-retention tests pass.

### Gate G — Recovery and SQL removal

- Restore drill passes; SQLx/PostgreSQL/migrations/legacy import are absent; full workspace checks pass; hosted mode may then be enabled.

## Plan review checklist

Before execution, reviewer should verify:

- all 70 old tables have an explicit disposition in `04-sql-to-mongo-disposition.md`;
- every collection has owner/partition, validator, indexes, retention, and delete behavior;
- every cross-document mutation names transaction/receipt/audit behavior;
- every generated/private byte has external storage and authorization lifecycle;
- no document has an unbounded array or plausible 16 MiB growth path;
- no encrypted/queryable field relies on unsupported uniqueness/TTL behavior;
- each unresolved product decision is assigned before its phase starts;
- rules/content expansion is not silently bundled into persistence replacement;
- final SQLectomy is gated by replacement tests, not performed first.
