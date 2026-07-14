# Manchester Arcana

Manchester Arcana is a web-based, AI-guided fantasy role-playing game built in Rust with [Leptos](https://book.leptos.dev/). It uses a deterministic rules engine for authoritative gameplay and treats the AI game master as a narrator and proposal generator—not as the owner of game state.

The repository now includes the full-stack foundation and the first persisted gameplay slice. It establishes:

- a Leptos 0.8 SSR/hydration app on Axum;
- d20 checks, ability scores, attacks, action economy, XP, and level progression in a framework-independent crate;
- a lazily created local campaign with a fixed level-1 hero and the authored `inspect-viaduct-runes` Wisdom check (proficient, DC 13);
- a strict shared command that carries IDs, expected revision, and an idempotency key—but no die, DC, ability, proficiency, modifier, or outcome;
- server-owned rules, dice, time, and event actors through `GameApplicationService`;
- atomic SQLite session-event, `AbilityCheckResolved` audit, and command-receipt persistence, with exact reload and retry replay;
- runtime text/image model profiles loaded with `dotenvy`;
- server-side AI and Markdown event-pack seams;
- planning for character creation, initiative, combat, damage/HP mutation, social play, and consent-aware real-life-inspired events.

This is Slice 1A, not the complete encounter slice: initiative, combat turns, attacks in a playable encounter, damage, and HP mutation remain pending.

## Run locally

Prerequisites are stable Rust, the `wasm32-unknown-unknown` target, and `cargo-leptos`.

```sh
rustup target add wasm32-unknown-unknown
cargo install --locked cargo-leptos
cp .env.example .env
mkdir -p data
cargo leptos watch
```

Open <http://127.0.0.1:6789>. Model calls are disabled in `.env.example`, so local development cannot accidentally make paid requests. Configure the `TEXT_LLM_*` and `IMAGE_LLM_*` profiles to enable generation.

`APP_ACCESS_MODE` defaults to `local`. Local mode must bind to a loopback address, denies browser framing, and the current game server functions accept only matching loopback HTTP `Host`/`Origin` authorities. These browser controls are not authentication and do not protect against another local process. The mode is deliberately unsuitable for reverse-proxy or remote exposure. Setting `APP_ACCESS_MODE=hosted` fails startup until authenticated browser sessions and campaign authorization exist.

Loading the page lazily creates the fixed local campaign and hero. Use **Inspect the runes**, then reload the page or use **Reload saved turn** to restore the exact committed dice and result without rerolling.

Operational probes are available at `GET /health/live` (process liveness) and `GET /health/ready` (database readiness).

## Workspace

```text
app/                  Leptos UI and typed server-function boundary
frontend/             Browser hydration entry point
server/               Axum/Leptos server entry point
crates/game-core/     Deterministic, framework-independent game rules
crates/game-server/   Application commands, configuration, AI, events, persistence
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
