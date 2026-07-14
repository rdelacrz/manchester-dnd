# System architecture

## Current modular workspace

Manchester Arcana starts as a Rust modular monolith using Leptos 0.8, Axum, SQLx, and SQLite. The implemented workspace is the architectural baseline:

```text
app/                 shared Leptos components, routes, and server functions
frontend/            browser/WASM hydration entry point
server/              Axum SSR binary and static-file integration
crates/game-core/    framework-independent rules, characters, dice, progression,
                     session DTOs, and declarative AI-GM proposals
crates/game-server/  server-only configuration, generation, GM orchestration,
                     event prompts, SQLite repositories, and boundary errors
content/             approved versioned rules/content sources
prompts/             system, theme, and private-event prompt roots
migrations/          SQLite migrations
public/ and style/    static assets and application styling
```

`game-core` remains deterministic and must not depend on Leptos, SQLx, a model SDK, the wall clock, or OS randomness. Callers inject dice through `DiceSource`; AI output is represented by declarative proposals and cannot apply itself. `game-server` owns secret-bearing and I/O adapters. `app` may share serializable view types but gates server-only bodies/dependencies behind the SSR feature.

As complexity grows, split feature modules inside these crates before creating more crates/services. Separate rules adapters, content validation, application use cases, telemetry, or provider workers only when ownership/build/runtime pressure is demonstrated; the conceptual boundaries in these plans are targets, not claims that those crates already exist.

## MVP request and turn flow

The current roll demonstration covers browser → Leptos server function → injected server dice → `game-core`. MVP extends that real vertical path as follows; repository, GM service, and configuration modules existing in isolation do not count as wired until this flow is exercised end to end.

```text
Browser intent
  → Leptos server function (a public HTTP API)
  → game-server authorization/local-mode policy and orchestration
  → optional typed AI intent proposal
  → game-core validation/resolution using server-supplied dice
  → revision-checked SQLite save + append-only turn audit
  → optional saved narration/image generation record
  → authorized DTO returned to the hydrated UI
```

The current persistence layer atomically creates a campaign with its declared party, atomically commits audited session/XP-character revisions, and stores ordered turn and generated-asset audits. MVP should preserve that working path while adding command idempotency, additional typed mechanical transitions, richer audit records, and durable generation jobs. An append-only domain-event stream and snapshots are an evolution path described in [persistence](05-persistence.md), not a description of today's database.

Only validated `game-core` results may update an authoritative campaign document. Generation can interpret a free-form intent before resolution or present committed facts afterward, but raw model output never becomes trusted state.

## Leptos 0.8 SSR and hydration boundaries

Use `cargo-leptos` with the official Axum integration. The [Leptos SSR guide](https://book.leptos.dev/ssr/index.html) identifies Axum as an officially supported/default choice. Pin the exact Leptos 0.8 minor in `Cargo.lock` and test upgrades.

### Server-rendered boundary

The server renders:

- application shell, routes, metadata, authentication/local-mode state, campaign list, and initial authorized campaign document;
- current scene, character summary, recent turn audit, generation placeholders, and accessible forms;
- not-found, access-denied, provider-degraded, and recoverable error states.

Prefer default out-of-order streaming SSR for independent read-only resources. Choose a more restrictive [SSR mode](https://book.leptos.dev/ssr/23_ssr_modes.html) only when the first safe render truly depends on data. Never place player data in a shared cross-user response cache.

### Hydrated client boundary

Hydration adds action/choice controls, dice animation, pending indicators, keyboard behavior, local presentation preferences, and generation-job updates. The browser never performs a trusted roll, validates a rules command, reads credentials/private prompts, or selects a private event source. Animation visualizes a server-returned roll record. Essential forms should progressively enhance where practical.

Shared Leptos components execute during SSR and again during hydration. Identical serialized inputs must produce identical markup: no render-time RNG, wall-clock branching, browser-only access, locale-dependent ordering, or model calls. Produce valid HTML and test for the mismatch modes in the official [hydration guidance](https://book.leptos.dev/ssr/24_hydration_bugs.html).

Use fixed-width serialized integers and stable enum tags across server-function boundaries. Keep SQLx, source-file access, credentials, and provider implementations out of the WASM dependency graph; scan release artifacts for canary secrets.

## Server functions are public APIs

Leptos [server functions](https://book.leptos.dev/server/25_server_functions.html) are convenient RPC endpoints, not a security boundary. Each server function must:

1. authenticate/authorize the caller or enforce the declared single-user local mode;
2. validate payload size, IDs, content type, command shape, and expected revision;
3. apply CSRF protections appropriate to cookie authentication;
4. rate-limit expensive/mutating calls and accept an idempotency key;
5. return a deliberately public, versioned DTO and stable safe error code;
6. omit secrets, source Markdown, raw provider responses, filesystem paths, and hidden state.

Mutation DTOs should carry `campaign_session_id`, `expected_revision`, and `idempotency_key`. Revision mismatch returns a conflict plus the newest safe view. Image generation returns a durable job ID once the jobs addition lands rather than holding an HTTP request open.

## Runtime configuration

`game-server::AppConfig::load` implements `dotenvy` loading: `APP_ENV_FILE` selects an explicit file; otherwise the nearest `.env` is optional, and existing process variables retain precedence. The Axum startup must call and provide this config before repository/provider/prompt services are used; defining the loader alone is not sufficient wiring. Production injects environment variables and does not require/copy a secret `.env` file.

Implemented profile names are:

```text
APP_ENV_FILE, RUST_LOG
DATABASE_URL, EVENT_PROMPT_DIR

TEXT_LLM_BACKEND, TEXT_LLM_BASE_URL, TEXT_LLM_API_KEY, TEXT_LLM_MODEL
TEXT_LLM_TIMEOUT_SECONDS, TEXT_LLM_TEMPERATURE, TEXT_LLM_MAX_OUTPUT_TOKENS

IMAGE_LLM_BACKEND, IMAGE_LLM_BASE_URL, IMAGE_LLM_API_KEY, IMAGE_LLM_MODEL
IMAGE_LLM_TIMEOUT_SECONDS, IMAGE_LLM_SIZE
```

`TEXT_LLM_*` and `IMAGE_LLM_*` are independent so text and image use different backends/models. MVP accepts the implemented `disabled` and `openai-compatible` backends; disabled is the safe local default. Validate URLs, timeouts, temperature/token bounds, model requirements, SQLite URLs, and prompt roots at startup. Store a non-secret profile fingerprint with retained output.

Commit `.env.example` with safe disabled/dummy values; ignore `.env*` secrets. Secret wrappers redact `Debug`/`Display`. Configuration changes take effect after restart in MVP. Audited hot routing among pre-approved configurations is later work.

See [`dotenvy`](https://docs.rs/dotenvy/latest/dotenvy/) for runtime-loading behavior.

## Error model

Use [`thiserror`](https://docs.rs/thiserror/latest/thiserror/) for typed boundaries. The current `game-core`/`game-server` families should evolve without collapsing into strings:

- core rules/state errors for invalid scores, levels, actions, targets, transitions, or unsupported mechanics;
- `ConfigError`, `EventPromptError`, `GenerationError`, `GameMasterError`, and `RepositoryError` in `game-server`;
- an application/transport mapping with safe code, retryability, and correlation ID.

Preserve causal sources internally with `#[source]`/`#[from]`. Client messages must not contain SQL, local paths, prompt text, credentials, provider bodies, or somebody else's identifiers. Expected errors render inline; unexpected failures return a correlation ID and structured redacted log.

## Generation execution

Text narration may run inline only under a strict deadline, after mechanics are safely saved. Image generation and slow text should use a durable SQLite job table with `queued`, `running`, `succeeded`, `failed`, and `cancelled` states, lease expiry, attempts, and retry time. Until that addition exists, generation is a bounded server operation and must not be described as crash-durable.

Provider adapters receive only minimized approved DTOs and cannot access repositories/files directly. Timeouts, bounded retries, concurrency/cost limits, circuit breaking, and deterministic fallbacks live in `game-server` orchestration.

## Persistence and deployment evolution

- **Current/MVP:** one Axum/Leptos server binary; SQLite database at `DATABASE_URL`; local protected prompt directory; local/static assets; optional worker loop in the same deployment when durable jobs arrive.
- **Hosted MVP hardening:** single-writer-aware transactions, WAL/busy-timeout policy, file permissions, consistent backup/restore, bounded worker concurrency, and persistent volume.
- **Scale trigger:** measured write contention, multi-instance failover needs, or operational limits justify a planned PostgreSQL migration. Add repository compatibility tests and migration/export tooling first.
- **Later scale:** independently deploy web/workers and move assets to an authorized object store. Introduce a queue or generation service only after database-job/egress pressure warrants it.

Threat controls are in [quality, observability, and security](09-quality-observability-security.md).
