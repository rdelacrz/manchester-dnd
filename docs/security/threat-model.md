# Private-MVP threat model

Status date: 2026-07-15. Scope: the supported loopback-only, single-user private
evaluation profile. This model does not authorize hosted, reverse-proxied,
multi-user, public-share, or public-distribution use.

## Security objective and attacker assumptions

The primary objective is to preserve authoritative mechanical state, private
campaign/source material, consent decisions, credentials, RNG key material,
generated artifacts, and recoverability. Optional generation may fail without
making a saved turn false, duplicate, or unreadable.

The model assumes the host operating system, the local operator account, and the
PostgreSQL/container administration boundary are trusted. A hostile website and
unauthenticated network peer are in scope. Another process running as the same
local user is **not** isolated by loopback Host/Origin checks and remains outside
the supported trust boundary. Hosted attackers and cross-account isolation are
addressed by failing hosted startup rather than by claiming controls that do not
exist.

## Assets and trust zones

| Zone | Assets | Allowed crossings |
| --- | --- | --- |
| Browser | Rendered campaign facts, local presentation preferences, intent-only commands | Same-origin HTTP to the loopback Axum server. No provider credential, raw source, RNG key, or hidden-state payload. |
| Axum/Leptos application | Validated commands, bounded DTOs, public error codes, correlation IDs | Application services only; generated prose is presentation data, never mechanical authority. |
| Deterministic engine | Pinned rules/content, legal state transitions, injected RNG cursor | Pure validated inputs in; canonical state, facts, and roll records out. No clock, database, provider, filesystem, or UI access. |
| PostgreSQL | Revisioned state, immutable audits, receipts, job/consent/artifact metadata | Least-privilege application role; explicit migration/backup/operator roles; encrypted transport when traffic leaves a trusted local boundary. |
| Protected files | RNG master key, selected/quarantined images, encrypted source/database vaults | Mode `0700` directories and `0600` regular files outside public roots; separate keys and operator processes. |
| Text/image providers | Minimized, purpose-specific bounded requests | Startup-approved HTTPS origin, no redirects, deadlines/limits/circuit breakers; disabled/fake adapters are the release default. |
| Offline inspiration admin | Decrypted source mount and participant verification evidence | Ephemeral read-only source access; writes only minimized facts, opaque IDs, digests, and closed consent policy to PostgreSQL. |
| Operator/recovery environment | Database/source vault keys, dumps, restore evidence | Separate private files and roles; never passed to the web, game, image worker, browser, CI artifact, or support bundle. |

## Entry points

- SSR/static `GET` routes and protected selected-image delivery;
- Leptos server functions carrying strict bounded intent-only DTOs;
- the confirmed canonical campaign-restore endpoint;
- startup environment/configuration and content-pack roots;
- PostgreSQL migrations and legacy one-time import;
- offline inspiration/source-vault and recovery-vault CLIs;
- text/image provider responses and generated image bytes; and
- canonical private exports restored by the local owner.

There are no uploads, public campaign links, remote provider-returned image fetches,
cookies, login routes, service workers, user-authored HTML, plugin execution, or
hosted account endpoints in the MVP.

## Threats, controls, evidence, and residual risk

| ID | Threat | Current controls and evidence | Residual / release decision |
| --- | --- | --- | --- |
| T01 | Accidental network or hosted exposure | Local mode validates a loopback bind; Host must be localhost/loopback; `APP_ACCESS_MODE=hosted` exits before bind. Provider-disabled smoke exercises forged Host and hosted startup. | Reverse proxying or remote exposure is unsupported. Do not infer trust from a hostname. |
| T02 | Malicious process on the same machine | Database/files use private roles and modes; keys are separate where practical. | Same-user local malware can drive or inspect a loopback browser session. Host hardening is an operator prerequisite, not an app guarantee. |
| T03 | CSRF, clickjacking, and cross-site drive-by mutation | Matching loopback Origin is required by game server functions and canonical restore; exact methods/content types; CSP `frame-ancestors 'none'`; `X-Frame-Options: DENY`; no cookies; bounded fixed-window local rate limits. | Cookie CSRF tokens, secure session rotation, and login throttling become mandatory before hosted mode can exist. |
| T04 | Stored/reflected XSS or injected active content | Leptos renders text with escaping; generated text has no raw-HTML path; packs and Markdown reject active markup/instructions; strict nonce CSP, `object-src 'none'`, `base-uri 'none'`, and `style-src 'self'`; no third-party scripts. | Re-review every new Markdown/HTML renderer or external asset before exposure. |
| T05 | Client/model forges rolls, actors, HP, XP, DC/AC, time, or revisions | Strict unknown-field DTOs accept intent only; application derives authority from pinned state; pure engine validates legal actions; PostgreSQL revision locks, immutable audit, and exact idempotency receipts; browser forgery tests. | None for the declared closed mechanic surface; adding a mechanic requires traceability and capability evidence. |
| T06 | ID enumeration or cross-owner object access | The local profile has one compiled campaign/owner boundary and protected image delivery rechecks that campaign. Public-share prefixes return `404`; hosted mode is disabled. | Cross-user authorization is not implemented and is a hard hosted-mode blocker, not a private-local claim. |
| T07 | Database corruption, partial write, replay, or lost acknowledgement | Atomic transactions, constraints, canonical document validation, immutable events, adjacent revisions/cursors, exact receipts, no automatic ambiguous retries, encrypted restore manifest comparison, legacy import drill. | Disk/controller/platform failure remains governed by operator backup quality and the manual local RPO; no external RTO is promised. |
| T08 | Secret leakage through WASM, SSR, logs, errors, headers, Git, or evidence | SSR/hydrate dependency separation, secret-redacting config errors, stable public codes, no body logging, dynamic canary scans over binary/site/responses/logs/evidence, checked-in signature scan. | Provider-side and Git-history scanning remain operator responsibilities. Rotate any suspected value before analysis. |
| T09 | RNG prediction, reroll, or audit substitution | Random mode-`0600` master key, derived opaque seed references, pinned `chacha20-v1`, canonical roll records with cursor ranges, replay from stored rolls, revision/idempotency paths spend no extra cursor. | Competitive multiplayer commitment/reveal is intentionally post-MVP. |
| T10 | Prompt injection, hidden-state leakage, or model authority escalation | Versioned typed proposal/narration schemas, untrusted-data delimiters, minimized visible state, closed action IDs, bounded repair, exact fact-fidelity validation, deterministic fallback, hostile evaluation corpus. | A newly enabled model/config must pass the same promotion corpus; model prose is never a trusted audit. |
| T11 | Provider SSRF, redirect, retention, or cost abuse | Startup-approved origin, remote HTTPS requirement, direct-IP restrictions, no redirects, no provider-returned URL fetch, time/byte/concurrency/circuit limits, durable budget receipts and hard caps. | Real-provider contractual privacy/output review is a deployment blocker; fake/disabled remains the accepted private-test default. |
| T12 | Job duplication, stale lease, crash, or cancellation race | Durable jobs/attempts, transactional `SKIP LOCKED` claim, lease token/expiry/heartbeat, attempt cap/backoff, idempotent enqueue, cancellation and expired-lease recovery tests. | Queue health must be monitored; operators must never repair job rows manually. |
| T13 | Malicious image bytes, decompression bomb, metadata, unauthorized artifact, or path traversal | Base64/byte/signature/MIME/dimension/pixel bounds, quarantine, decode/re-encode metadata stripping, canonical protected paths, digest verification, selected web/thumbnail-only route, no original route, private cache headers. | Semantic safety of a real provider also depends on its approved moderation; unapproved providers stay disabled. |
| T14 | Private source, identity, consent, or derived-output misuse | Separate encrypted source vault/admin role; PII/sensitivity quarantine; minimized facts only; exact participant/source/audience/media/expiry consent; deterministic zero-probability filtering; immediate pause/veto/revoke/delete/global switch; raw-body canary drills. | Real sources remain off until represented-user testing and provider approval; urgent harm uses the operator escalation path, not the game. |
| T15 | Export, backup, or restore leaks/reintroduces deleted data | Private canonical/readable exports exclude credentials/raw sources/other consent; exact content type, same origin, confirmation, idempotency and size limits; chunked authenticated recovery vault; 30-day backup and 35-day tombstone procedure. | Export files are owner-controlled sensitive documents and must use encrypted storage and secure deletion. |
| T16 | Dependency, content, license, or build compromise | Locked Rust/npm dependencies; pinned toolchain/actions/container images; minimal non-root runtime; mechanic/pack digest/provenance gates; SBOM/advisory/license release gates. | Signing depends on release infrastructure. Public distribution is blocked by Q15/Q16 and human review. |
| T17 | Resource exhaustion and denial of service | Request/restore/image-read rate windows, request/body/string/collection/file/image limits, database pool/timeouts, generation concurrency/budgets, compressed-input rejection, circuit breaking. | The fixed limiter is deliberately process-local for one user; distributed limits are required before hosted deployment. |
| T18 | Operational telemetry becomes behavioral surveillance | Q18 permits bounded enums/counts/latency/health only; no campaign/user IDs or bodies as labels; database operations snapshot is body-free; canary scans cover retained evidence. | Any client analytics or behavioral funnel requires a new explicit consent/policy decision. |

## Abuse and safety cases

- The product is adults-only for private testing. It is not an emergency service.
- Current crisis, minors, real-person likenesses, and unconsented health, trauma,
  sexual, criminal, financial, employment, address/contact, or relationship
  secrets are prohibited from private inspiration.
- An imminent-harm report stops generation and follows the private-test operator's
  out-of-band escalation policy. The app does not attempt automated diagnosis.
- Player veto/revocation never requires a reason and never alters committed
  mechanics; related presentation work is hidden/cancelled/redacted according to
  the stored policy.

## Review triggers

Re-run this model before adding hosted identity, a proxy/listener beyond loopback,
cookies, another campaign owner, uploads, public sharing, a new parser/renderer,
provider/region, worker role, object store, source category, content pack, model
capability, or release distribution profile. Link each change to penetration,
privacy, recovery, authorization, and provenance evidence proportionate to the new
crossing.

Current hard holds: external Q16 name/domain/trademark clearance; explicit public
code/content/contribution licenses for any public distribution; real-provider terms
approval; real-device/assistive-technology evidence; and represented-participant
testing before real private sources are enabled.
