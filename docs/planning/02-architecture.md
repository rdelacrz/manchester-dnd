# System architecture

## Modular workspace

Manchester Arcana is a Rust modular monolith using Leptos 0.8, Axum, MongoDB, and
optional DragonflyDB:

```text
app/                  Leptos views, server-function boundary, SSR/hydration
crates/game-core/     deterministic mechanics and serializable domain contracts
crates/game-server/   application orchestration, auth, persistence, generation,
                      content, protected files, and boundary errors
content/              approved versioned rules/content packs and provenance
prompts/              system/theme/private-inspiration conventions
public/ and style/    static assets and styling
```

`game-core` does not depend on Leptos, MongoDB, provider SDKs, wall clock, filesystem,
or OS randomness. Callers inject dice/time and AI output is a declarative proposal
that cannot mutate state. `game-server` owns all I/O, credentials, tenant checks, and
secret-bearing adapters. Browser/WASM code never links persistence or providers.

## Authoritative request path

```text
Leptos command DTO (intent + expected revision + idempotency key)
  → authentication/session + CSRF/origin validation
  → account/campaign membership authorization
  → GameApplicationService selects trusted rules, actor, time, dice
  → game-core validates and resolves deterministic mechanics
  → short MongoDB transaction compare-and-swaps state and appends
    immutable audit/event + exact command receipt
  → optional Dragonfly invalidation/publication (failure degrades safely)
  → bounded public response DTO
```

The client cannot submit authoritative dice, DC/AC, modifiers, HP, XP, actors,
timestamps, or generated facts.

## Data services

### MongoDB

MongoDB is the only system of record and runs as a replica set. The schema catalog
owns 34 collections and 87 indexes. Multi-document mutations use transactions,
revisions, unique idempotency constraints, and tenant-scoped filters. Schema mutation
uses a separate admin credential and `mongo-admin`; the app role is data-only.

### DragonflyDB

Dragonfly provides bounded session-cache entries, throttling acceleration,
invalidation, and pub/sub. Keys contain opaque/HMAC identifiers, never raw email,
PII, prompt/source text, provider data, or mechanical authority. Cache errors are
misses/no-op publication and fall back to MongoDB.

## Browser and authentication boundary

SSR and hydration use identical serialized inputs: no render-time randomness,
wall-clock branching, locale-dependent ordering, browser-only access during SSR, or
model calls.

Server functions are public APIs. They require exact methods/content types, bounded
DTOs, authentication, CSRF, canonical origin/Host policy, campaign authorization,
rate limits, and private cache controls. Errors expose stable public codes and
correlation IDs, not internals or secrets.

Opaque server-side sessions are authoritative in MongoDB. Dragonfly may cache only
bounded session validity and cannot extend expiry/revocation. Signup uses one-use
operator-issued access codes. Hosted mode requires HTTPS origin, secure cookies,
MongoDB TLS/authentication, and explicit email-HMAC/data-encryption keys.

## Configuration

Important groups:

```text
APP_ACCESS_MODE, LEPTOS_SITE_ADDR, LEPTOS_SITE_ROOT, RUST_LOG
MONGODB_URI, MONGODB_SCHEMA_URI, MONGODB_DATABASE, MONGODB_SCHEMA_POLICY
DRAGONFLY_ENABLED, DRAGONFLY_URL, DRAGONFLY_POOL_SIZE, DRAGONFLY_TIMEOUT_MILLISECONDS
AUTH_COOKIE_SECURE, AUTH_PUBLIC_ORIGIN, AUTH_EMAIL_LOOKUP_KEY,
AUTH_DATA_ENCRYPTION_KEY
TEXT_LLM_*, IMAGE_LLM_*, INSPIRATION_ENABLED, EVENT_PROMPT_DIR
RNG_MASTER_KEY_FILE, IMAGE_ARTIFACT_ROOT
```

Startup validates bounds, credential/TLS policy, safe roots, provider origins,
model/time/token limits, and mode-specific auth. Secret values are redacted.

## Generation execution

Mechanics commit before optional narration. Text/image requests use minimized bounded
DTOs, startup-approved HTTPS providers, no redirects, deadlines, response-size
limits, concurrency/budget controls, and deterministic fallback.

Durable generation jobs/reservations/presentations/assets live in MongoDB. Workers
lease with atomic compare-and-swap/transactions and idempotency; retries never repeat
a committed provider effect. Generated files are non-public until validated and
selected. Dragonfly notification loss falls back to bounded MongoDB polling.

## Protected inspiration boundary

Only the offline `inspiration-admin` may read decrypted Markdown. It validates and
minimizes approved input into neutral runtime facts and consent evidence stored in
MongoDB. Web/game/image processes cannot mount or decrypt source files. Revocation,
veto, deletion, and campaign policy are authoritative Mongo state; Dragonfly never
stores private source content.

## Deployment evolution

- **Local:** loopback Axum/Leptos, loopback MongoDB replica set, optional Dragonfly,
  providers/inspiration disabled by default.
- **Hosted:** authenticated sessions, cross-user authorization, HTTPS canonical
  origin, secure cookies, least-privilege Mongo credentials, TLS, reviewed schema
  reconciliation, backup/restore, and bounded worker concurrency.
- **Scale trigger:** measured database size/throughput, transaction contention,
  replication/failover requirements, queue pressure, or artifact egress.
- **Later:** managed Mongo topology, independently deployed workers, approved object
  storage, or external queue only when measurements justify them.

## Health

`GET /health/live` proves process responsiveness. `GET /health/ready` proves
MongoDB/schema readiness. Dragonfly and providers are reported separately because
both are optional degraded paths, not prerequisites for authoritative gameplay.
