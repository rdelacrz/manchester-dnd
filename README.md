# Manchester Arcana

> [!WARNING]
> This is a mostly vibe-coded app that was made for kicks and giggles. It is very likely to be error-prone!

Manchester Arcana is a web-based, AI-guided fantasy role-playing game built in Rust with [Leptos](https://book.leptos.dev/). It uses a deterministic rules engine for authoritative gameplay and treats the AI game master as a narrator and proposal generator—not as the owner of game state.

The repository now includes a playable, persisted private/local game path. It establishes:

- a Leptos 0.8 SSR/hydration app on Axum;
- a resumable, server-validated fighter/wizard hero creator and deterministic level-1-to-2 advancement;
- an authored exploration check followed by a complete turn-based encounter with initiative, movement, action economy, attacks, spells/class actions, HP/death-save transitions, objectives, victory/defeat, and reward handling;
- server-owned ChaCha20 dice with canonical stored roll records, opaque seed references, exact cursor spans, immutable audits, optimistic revisions, and idempotent replay;
- strict intent-only browser commands: the client cannot submit dice, AC/DC, modifiers, damage, HP, XP, actors, or timestamps;
- constrained typed-GM interpretation and mechanics-first narration with deterministic fake/disabled paths, durable job/presentation provenance, exact lost-response recovery, and bounded presentation versions;
- local campaign list/resume, explicit play sittings, ordered history, archive/delete, private readable export, and canonical restore;
- immutable content-pack, capability, provenance, and SRD traceability gates; and
- a default-off, canonical-root private-inspiration ingestion boundary that quarantines uncertain input and never retains raw source Markdown in game state.

This remains a private working build. Hosted identity, real-provider approval, the consented-inspiration player workflow, asynchronous scene images, production backup/PITR, public branding clearance, and final release evidence remain fail-closed or unfinished; see [`docs/CHECKLIST.md`](docs/CHECKLIST.md) for the exact gate.

## Run locally

Prerequisites are the repository's pinned Rust toolchain, the `wasm32-unknown-unknown` target, `cargo-leptos` 0.3.7, Docker Compose, MongoDB 8 replica-set support, and optional DragonflyDB. The included Compose services provide both data services.

```sh
rustup toolchain install 1.90.0 --profile minimal --component rustfmt,clippy --target wasm32-unknown-unknown
cargo install --locked --version 0.3.7 cargo-leptos
docker compose up -d --wait mongodb dragonfly
cargo run --locked -p manchester-dnd-server --bin mongo-admin -- schema apply
cargo leptos watch
```

Open <http://127.0.0.1:6789>. Model calls and private inspiration are disabled in `.env.example`, so local development cannot accidentally make paid requests or read a private source tree. Keep those gates off unless their documented prerequisites are satisfied.

`APP_ACCESS_MODE` defaults to `local`. Local mode must bind to a loopback address and denies browser framing. Authentication uses MongoDB-backed accounts/sessions with one-use signup access codes. Hosted mode additionally requires explicit secure cookies, a canonical HTTPS origin, MongoDB TLS/authentication, and separate email-lookup/encryption keys.

Loading the page creates or resumes the local campaign. Complete the guided hero creator, use **Inspect the runes**, then play only the legal actions rendered by the encounter. Reloading at any point projects the stored revision, dice, HP, resources, and outcome without rerolling.

Operational probes are available at `GET /health/live` (process liveness) and `GET /health/ready` (database readiness).

Repository/application tests use randomly named isolated MongoDB databases on the local replica set. The test URI must be an explicit loopback test administrator because each test applies validators/indexes and drops only its safeguarded random database:

```sh
MONGODB_TEST_URI='mongodb://root:<local-test-password>@127.0.0.1:27017/?authSource=admin&replicaSet=rs0&directConnection=true' \
  cargo test --locked --workspace -- --test-threads=1
```

The credentials in `.env.example` and `compose.yaml` are local-development defaults only. Deployments must inject secret-managed least-privilege MongoDB/Dragonfly credentials and require certificate-validated TLS outside loopback. MongoDB is authoritative; DragonflyDB may be flushed or lost without losing durable state.

## Workspace

```text
app/                  Leptos UI, hydration, and Axum/Leptos server entry points
app/src/components/   Reusable UI components and server-function boundary
app/src/views/        Route-level views
crates/game-core/     Deterministic, framework-independent game rules
crates/game-server/   Application commands, configuration, AI, events, persistence
crates/game-server/src/persistence/ MongoDB validators, indexes, auth, and schema reconciliation
prompts/               AI-GM, theme, and private event-pack conventions
docs/planning/         Product, architecture, safety, and delivery plans
```

## Useful checks

```sh
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo leptos build --release --bin-cargo-args=--locked --lib-cargo-args=--locked
python3 scripts/validate_mechanic_traceability.py
npm run test:browser
```

## Rules and content

The default ruleset targets the 2014/2018-era fifth-edition mechanics under the SRD 5.1 Creative Commons release. The user-linked 2018 Basic Rules are a design reference, not the source for bundled text or art. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [the licensing plan](docs/planning/10-licensing-and-provenance.md).

“Manchester Arcana” is a working title. The project does not use Wizards of the Coast logos, setting lore, or non-SRD product identity.
