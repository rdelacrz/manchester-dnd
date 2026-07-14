# Manchester Arcana

Manchester Arcana is a web-based, AI-guided fantasy role-playing game built in Rust with [Leptos](https://book.leptos.dev/). It uses a deterministic rules engine for authoritative gameplay and treats the AI game master as a narrator and proposal generator—not as the owner of game state.

The repository is an early vertical-slice foundation. It establishes:

- a Leptos 0.8 SSR/hydration app on Axum;
- d20 checks, ability scores, attacks, action economy, XP, and level progression in a framework-independent crate;
- runtime text/image model profiles loaded with `dotenvy`;
- server-side AI, SQLite persistence, and Markdown event-pack seams;
- versioned campaign/session data and structured AI-GM proposals;
- planning for character creation, combat, exploration, social play, and consent-aware real-life-inspired events.

## Run locally

Prerequisites are stable Rust, the `wasm32-unknown-unknown` target, and `cargo-leptos`.

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked cargo-leptos
cp .env.example .env
mkdir -p data
cargo leptos watch
```

Open <http://127.0.0.1:3000>. Model calls are disabled in `.env.example`, so local development cannot accidentally make paid requests. Configure the `TEXT_LLM_*` and `IMAGE_LLM_*` profiles to enable generation.

## Workspace

```text
app/                  Leptos UI and typed server-function boundary
frontend/             Browser hydration entry point
server/               Axum/Leptos server entry point
crates/game-core/     Deterministic, framework-independent game rules
crates/game-server/   Configuration, AI adapters, event loading, persistence
migrations/           SQLite schema
prompts/               AI-GM, theme, and private event-pack conventions
docs/planning/         Product, architecture, safety, and delivery plans
```

## Useful checks

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo leptos build
```

## Rules and content

The default ruleset targets the 2014/2018-era fifth-edition mechanics under the SRD 5.1 Creative Commons release. The user-linked 2018 Basic Rules are a design reference, not the source for bundled text or art. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and [the licensing plan](docs/planning/10-licensing-and-provenance.md).

“Manchester Arcana” is a working title. The project does not use Wizards of the Coast logos, setting lore, or non-SRD product identity.
