# Quality, observability, and security

## Quality strategy

Correctness is layered: mechanical outcomes must be exact and replayable; model presentation must be schema-valid, faithful, safe, and replaceable; the web boundary must preserve authorization and hydration invariants.

### Test portfolio

| Layer | Required tests |
| --- | --- |
| Domain/rules unit | Table-driven mechanic vectors linked to source keys; modifier/effect ordering; legal/illegal transitions; level-up prerequisites |
| Property/model | HP/resource bounds, no action after budget spent, event invariants, advantage/disadvantage laws, command determinism, arbitrary valid state-machine sequences |
| RNG/dice | Pinned algorithm known-answer tests; parser limits/overflow; roll-record totals; non-flaky statistical smoke checks separate from exact conformance |
| Save/history compatibility | Golden campaign/character documents and turn audits from every supported schema/rules/content version; canonical state hashes; migration semantics; event-stream fixtures only after that evolution exists |
| Content/pack | Schema, dependency, path/digest/provenance, referential integrity, capability reachability, instantiate every offered build/entity |
| Persistence | Real MongoDB validators/indexes/transactions, rollback, revision conflicts, idempotency races, transient-label handling, tenant isolation, backup restore, expired job leases, Dragonfly degradation |
| Application/API | Authentication/authorization per server function, safe error mapping, size/rate limits, CSRF, hidden-field/ID forgery, cancellation/timeouts |
| Leptos UI | SSR render, hydration with zero warnings, progressive forms, stale-revision recovery, keyboard/focus, accessible names/status updates |
| Browser E2E | Create → play → roll → reload → level → export; provider degradation; worker restart; two-user isolation |
| Generation contract | Deterministic fake adapters for CI; strict unknown-field rejection; raw DC/AC/modifier/XP injection cases; known ID allowlists; schema repair/fallback; fact fidelity; hidden-information and prompt-injection corpus |
| Safety/privacy | Consent eligibility properties, revocation races, identifier leakage scans, Markdown hostile corpus, artifact redaction/deletion |
| Fuzz | Dice expressions, pack/Markdown parsers, proposal JSON, public IDs/routes, event upcasters and canonical deserialization |
| Operations | Backup restore, rollback/read compatibility, provider chaos, storage failure, rate-limit/load/soak, incident runbooks |

Do not assert exact creative prose from a live model in normal CI. Model promotion evaluations run on synthetic/non-private fixtures with schema validity, factual consistency, safety/privacy, latency, and cost thresholds. Keep provider-contract smoke tests opt-in and budgeted.

Current evidence covers strict command/result decoding, tamper rejection, deterministic injected rolls, same-key replay without reroll, concurrent independent Mongo repository handles, changed-key conflicts, stale revisions, transaction rollback, exact stored-result reload, safe error mapping, account/campaign isolation, and Origin/Host/CSRF checks. Live smoke verification also exercises health and commit/reload over HTTP. This is not yet full release evidence.

### Rules traceability

Maintain a machine-readable matrix:

```text
mechanic_id → rules profile/source locator → implementation symbol
            → supported content references → conformance/property tests
```

Source locators point to licensed SRD 5.1 sections/pages without copying long prose. CI fails if shipped content requires an unknown capability, a mechanic lacks tests, or an attribution/provenance entry is missing.

### Definition of done

A change is done only when its observable behavior, error cases, authorization, schema/versioning impact, telemetry/redaction, accessibility, and relevant documentation are tested. Bug fixes begin with a failing regression test. Non-deterministic flakes are defects, not retries to normalize.

## Observability

Use structured Rust tracing and open telemetry-compatible traces/metrics. Propagate a correlation ID through HTTP request, application command, database transaction, event IDs, generation job, provider attempt, and artifact. Use opaque campaign/user IDs only where operationally necessary; never attach prompt text, narration, Markdown, names, access tokens, or full model responses as span fields.

### Signals

- **Web:** request rate/latency/status by low-cardinality route, active requests, payload rejection, CSRF/auth failures.
- **Game:** command latency/outcome by command type, revision conflicts, idempotency hits, unsupported-mechanic rate, turns committed, document load/history duration and hash failures.
- **Generation:** queue age/depth, lease expiry, provider latency/status, timeout/circuit state, schema/repair/fallback rate, tokens/images and estimated cost by purpose/config fingerprint.
- **Storage:** pool saturation, query/transaction latency, migration status, object errors, backup age and restore-test result.
- **Safety/security:** aggregate policy rejection, veto/redaction workflow health, rate-limit triggers, anomalous authorization denials; no source/event text or sensitive high-cardinality labels.
- **Client:** sampled hydration failures, recoverable command conflicts, asset-load failures, and web vitals under an explicit privacy-respecting analytics policy.

Dashboards separate core mechanical availability from optional provider availability. Initial service objectives for private MVP should measure: no acknowledged committed-turn/document loss; deterministic resume success; core command latency excluding generation; and fallback success during provider outage. Set numeric SLOs after Slice 1 load measurements rather than inventing unsupported targets.

Alerts must be actionable and linked to runbooks: turn save/load or audit failure, database/storage exhaustion, migration mismatch, old backup, job queue age, widespread provider failure, cost-spend anomaly, authorization-denial spike, secret/privacy leak signal, or artifact-policy bypass.

### Error and log hygiene

`thiserror` types preserve internal causal chains. Transport responses expose stable codes, retry guidance, and a correlation ID. Redaction tests snapshot structured logs for representative failures and scan for configured canary secrets/private identifiers. Sampling never drops security/audit events, and production debug capture is time-bound, encrypted, access-audited, and off by default.

## Security model

### Protected assets

- accounts, browser sessions, campaign/character state, rolls and private narration;
- consent records and real-life prompt sources;
- model/database/object-store credentials and signing/encryption keys;
- licensed pack sources, generated artifacts, provenance, and usage budget;
- integrity of the rules engine, revisioned campaign/character documents, turn audits, migrations, and worker queue.

Expected adversaries include an unauthenticated internet client, a malicious campaign member, hostile free-form/Markdown/pack content, automated cost/denial abuse, a compromised provider response, and accidental operator leakage. MVP does not claim defense against a fully compromised host/database administrator.

### Controls by boundary

**Browser and HTTP**

The current local boundary is intentionally narrower than the hosted controls below: the binary binds to loopback, responses set CSP `frame-ancestors 'none'` plus `X-Frame-Options: DENY`, both game functions require matching loopback HTTP Host/Origin, and hosted mode fails startup. These browser controls are not authentication and do not defend against another process on the same machine; cookies, user identity, object authorization, CSRF tokens, and remote exposure remain unimplemented.

- Secure, HttpOnly, SameSite cookies; rotation/revocation; short appropriate lifetimes; TLS and strict transport policy.
- Authentication plus object-level authorization inside every server function; deny by default and test cross-campaign IDs.
- CSRF protection for cookie-authenticated mutations, explicit methods/content types, origin checks as defense in depth.
- Escape all text; sanitize generated/imported Markdown to a small allowlist; restrictive CSP; no untrusted script/style/HTML.
- Request/body/file limits, timeouts, per-user/IP/campaign cost-aware rate limits, and idempotency keys.

**Leptos SSR/WASM**

- Server-only Cargo features for database, credentials, prompt sources, provider SDK secrets, and admin logic.
- Build-time scans of WASM/JS/source maps and rendered bootstrap data for secret canaries/private fields.
- Deterministic shared rendering to prevent hydration divergence; authorize before loading SSR data and avoid cross-user shared caches.

**Application, MongoDB, and DragonflyDB**

- Typed BSON, least-privilege app/schema roles, managed validators/indexes, tenant-scoped filters, expected-revision checks, immutable turn/security audits, integrity hashes, bounded connection/transaction concurrency, and fail-safe disposable-cache behavior.
- Centralized authorization and safe-view construction; providers never receive repository handles.
- Treat connection URLs as credentials, require TLS across non-local boundaries, segregate environments/roles, audit privileged access, encrypt backups, and test credential rotation plus logical/point-in-time restore as required by the recovery objective.

**Generation and egress**

- Treat player/model/provider content as untrusted; typed schemas/allowlists and no autonomous tools.
- Allowlist provider hosts, block arbitrary URL fetching/SSRF, cap redirects/response sizes, enforce deadlines/concurrency/budgets.
- Verify returned image MIME by bytes, dimensions and decompression limits; strip metadata; quarantine before publication.
- Review provider retention/training/region settings and sign appropriate agreements for the deployment before private material is enabled.

**Content ingestion**

- Canonical root confinement, reject traversal/symlinks where policy requires, file/count/size/archive expansion limits, digest everything.
- No executable content or remote resource loading; strict manifest schema, capability/provenance/license gates, quarantine invalid packs.
- Inspiration Markdown goes through the stronger consent and identifier pipeline in [06-consent-privacy-safety](06-consent-privacy-safety.md).

### Supply chain and release

- Commit `Cargo.lock`; pin deployment toolchains and actions by immutable versions/digests.
- Run vulnerability, license, secret, and dependency-policy scans; review `cargo audit`/RustSec findings and use a `cargo-deny`-style allow/deny policy.
- Generate an SBOM and provenance for release containers; minimize runtime image/user privileges and make the filesystem read-only except declared paths.
- Protect release branches/tags, require reviewed migrations, sign artifacts where infrastructure supports it, and document emergency dependency/provider disablement.

## Security/privacy release gates

Before MVP exposure beyond developers:

- threat model reviewed with data-flow diagrams and abuse cases;
- cross-tenant authorization suite, CSRF/XSS/SSRF/prompt-injection tests, secret/client artifact scan, dependency/license scan all pass;
- backup restore, key/credential rotation, provider outage, artifact quarantine, consent revocation, and privacy deletion runbooks exercised;
- no unresolved critical/high finding without explicit owner, compensating control, and time-bounded acceptance;
- reporting contact and incident process are visible to the intended users.
