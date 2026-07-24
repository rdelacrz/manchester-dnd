# Security threat model

Status date: 2026-07-24. Scope: authenticated local and fail-closed hosted profiles.
Public campaign sharing/distribution remains out of scope.

## Objectives and trust assumptions

Protect authoritative mechanics, account/campaign isolation, credentials, private
source/consent material, RNG keys, artifacts, and recoverability. Optional providers
or DragonflyDB may fail without making a saved turn false, duplicate, inaccessible,
or unauthorized.

The host OS/operator and MongoDB/container administration boundary are trusted. A
hostile website, unauthenticated network peer, malicious account, and cross-account
ID enumerator are in scope. Same-user local malware is outside the application trust
boundary and can inspect or drive a loopback browser session.

## Trust zones

| Zone | Assets and permitted crossings |
| --- | --- |
| Browser | Rendered authorized facts and intent-only commands; same-origin HTTPS/loopback HTTP only; no provider/database credentials, RNG keys, source text, or hidden state. |
| Axum/Leptos | Authenticates Mongo-backed sessions, validates CSRF/origin, tenant scope, bounded DTOs, and emits public errors/correlation IDs. |
| Deterministic engine | Pinned rules/content and injected dice/time in; canonical state/facts/roll records out; no database, provider, filesystem, UI, or ambient randomness. |
| MongoDB | Sole authority for accounts/sessions, campaigns, mechanics, audits, receipts, jobs, consent, and artifact metadata; replica-set transactions, validators/indexes, tenant filters, least-privilege app/schema roles, TLS outside loopback. |
| DragonflyDB | Disposable bounded session/throttle cache and pub/sub; opaque/HMAC keys only; never grants authority or stores PII, source text, provider payloads, or mechanics. |
| Protected files | RNG master key, selected/quarantined images, encrypted source/recovery vaults; private modes outside public roots and separate operator keys. |
| Providers | Minimized purpose-specific requests to startup-approved HTTPS origins; no redirects; strict time/byte/concurrency/budget controls. |
| Offline inspiration admin | Ephemeral decrypted source access; writes only reviewed minimized facts, opaque IDs, digests, and consent state to MongoDB. |
| Recovery operator | Schema/root credential, recovery/source keys, archives, and evidence; never supplied to web/worker/browser/CI artifacts. |

## Entry points

- public SSR/static routes, login/signup/logout, and selected-image delivery;
- Leptos server functions with strict intent-only DTOs;
- canonical owner-authorized export/restore;
- configuration, content/provenance packs, and managed Mongo schema catalog;
- offline inspiration/source-vault/recovery CLIs;
- provider responses and generated image bytes.

There are no user-authored HTML/plugin execution paths or public campaign links.

## Threat register

| ID | Threat | Controls / residual decision |
| --- | --- | --- |
| T01 | Accidental exposure | Local mode requires loopback; hosted mode requires HTTPS canonical origin, secure cookies, auth keys, Mongo auth/TLS. Reverse proxying local mode is unsupported. |
| T02 | Account takeover/session theft | Argon2id passwords; one-use signup access tokens; opaque server sessions; HttpOnly/SameSite cookies; idle/absolute expiry, rotation/revocation; HMAC throttle keys; Dragonfly cache cannot extend Mongo authority. |
| T03 | CSRF/clickjacking | Exact methods/content types, CSRF token, canonical Origin/Host, SameSite, CSP `frame-ancestors 'none'`, `X-Frame-Options: DENY`. |
| T04 | XSS/active content | Leptos escaping; no raw generated HTML; pack/Markdown active-markup rejection; nonce CSP, no third-party scripts; provider image decode/re-encode. |
| T05 | Forged mechanics | Unknown-field-denying intent DTOs; server-owned actor/time/dice/rules; pure engine; Mongo revision CAS, atomic audits and exact idempotency receipts. |
| T06 | ID enumeration/cross-tenant access | Every repository/API/artifact operation scopes account owner or active campaign membership; unauthorized existence is not disclosed; cross-user contract/browser tests required for each new endpoint. |
| T07 | Partial write/replay/lost acknowledgement | Replica-set transactions, validators, unique indexes, exact receipts, adjacent revisions/cursors; only explicit transient transaction-label retries; unknown commit retries commit only. |
| T08 | Secret leakage | SSR/WASM dependency separation, redacted config/errors, no body logging, private cache headers, canary scans over artifacts/logs/evidence; rotate suspected values. |
| T09 | RNG prediction/reroll | Mode-`0600` master key, derived opaque seed references, pinned algorithm, canonical cursor ranges, exact replay spends no extra cursor. Competitive commitment/reveal is post-MVP. |
| T10 | Prompt injection/model authority | Typed proposals, untrusted-data delimiters, minimized visible state, closed action IDs, bounded repair, fact validation, deterministic fallback; prose is never audit authority. |
| T11 | Provider SSRF/retention/cost | Approved HTTPS origin, no redirects/direct-IP ambiguity, no returned-URL fetch, deadlines/size/concurrency/circuit/budget limits; contractual review before enablement. |
| T12 | Job duplicate/stale lease | Durable Mongo jobs/reservations/attempts, transactional/CAS lease token and expiry, idempotent enqueue, cancellation/reclamation tests; Dragonfly notification loss polls Mongo. |
| T13 | Malicious image/path | Base64/signature/MIME/dimension/pixel bounds, quarantine, decode/re-encode metadata stripping, protected canonical paths, digest verification, selected derivative-only route. |
| T14 | Private source/consent misuse | Separate source vault/admin process, PII quarantine, minimized facts, exact participant/source/media/audience/expiry grants, immediate pause/veto/revoke/delete/global gate. |
| T15 | Export/backup reintroduces data | Owner authorization, bounded canonical/readable formats, exclusions, authenticated 30-day recovery vault, 35-day deletion tombstones, isolated restore/schema/count verification; Dragonfly excluded. |
| T16 | Database/cache privilege escalation | App/schema Mongo credentials separated; app cannot create collections/manage roles; URI/TLS/database allowlists; Dragonfly authenticated and non-authoritative. |
| T17 | Resource exhaustion | Request/body/string/list/file/image limits, Mongo/Dragonfly pool/timeouts, generation budgets, rate limits, compressed-input rejection, circuit breaking. Distributed limits are required at scale. |
| T18 | Telemetry surveillance | Bounded enums/counts/latencies/health only; no campaign/user identifiers or bodies as labels; operational evidence is body-free. |
| T19 | Supply-chain/content compromise | Locked dependencies/toolchain/actions/images, minimal non-root runtime, content digest/provenance/capability gates, SBOM/advisory/license release checks. |

## Safety and incident handling

Private inspiration excludes minors, imminent crises, real-person likenesses, and
unconsented sensitive health/trauma/sexual/criminal/financial/employment/contact/
relationship facts. Veto/revocation requires no reason and never changes committed
mechanics; related presentation is cancelled/hidden/redacted by policy.

On suspected compromise: stop optional workers and mutation, preserve redacted
evidence, revoke/rotate affected credentials, create an encrypted Mongo recovery
vault when safe, restore only into isolation, verify schema/state/authorization, and
resume after an idempotent read and recovery drill. Dragonfly data is flushed—not
restored.

Review this model whenever hosted/public sharing, uploads, new providers/renderers,
multi-account features, new source categories, topology changes, or new retention
classes are introduced.
