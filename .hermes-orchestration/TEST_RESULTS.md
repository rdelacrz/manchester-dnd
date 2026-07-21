# Rewrite Part 1 — Test Results

## Baseline / latest

| Area | Command | Result |
|---|---|---|
| PostgreSQL readiness | `docker compose ps postgres` + `docker compose exec -T postgres pg_isready -U manchester_arcana -d manchester_arcana` | PASS — healthy; accepting connections |
| Full Rust workspace | `DATABASE_URL=… cargo test --locked --workspace` | PASS — 393 tests: app 7, core 170, server 211, integration 3, admin 2; all doc tests pass |
| App unit/SSR | `cargo test --locked -p manchester-dnd-app` + `cargo build --locked -p manchester-dnd-app --features ssr` | PASS — 7 lib tests; SSR build succeeds |
| Clippy | `cargo clippy --locked --workspace --all-targets -- -D warnings` | PASS — zero warnings |
| Formatting | `cargo fmt --all -- --check` | PASS |
| Mechanic traceability | `python3 scripts/validate_mechanic_traceability.py` | PASS |

## Test growth from rewrite work

| Crate | Before | After | Delta |
|---|---|---|---|
| game-core | 169 | 170 | +1 (Task 9: campaign instance advancement independence) |
| game-server | 193 | 211 | +18 (Task 10: character-library application service + two-account isolation tests) |
| app | 7 | 7 | 0 |
| **Total** | **372** | **393** | **+21** |

## Git damage repair (2026-07-20)

Controller ran `git checkout e7facd5 -- <files>` which stripped module declarations from `views/mod.rs`, `repository.rs`, `lib.rs`. Repaired by restoring:
- `app/src/views/mod.rs` — 6 missing view module declarations
- `crates/game-server/src/repository.rs` — `mod auth;` and `mod player_characters;`
- `crates/game-server/src/lib.rs` — `pub mod auth;` and auth type re-exports
- `crates/game-core/src/lib.rs` — `pub mod player_character;` and re-exports
- `crates/game-core/src/hero.rs` — `require_schema`/`validate_id` made `pub(crate)`
- `app/src/lib.rs` — `#[cfg(feature = "ssr")] pub(crate) mod auth_boundary;`
- `app/src/app.rs` — `LocalGame` → `use crate::views::home::Home as LocalGame;`
- `crates/game-server/Cargo.toml` — added argon2, hmac, password-hash deps
- `app/Cargo.toml` — added sha2, subtle as SSR deps
- `crates/game-server/src/config.rs` — added AuthenticationConfig + AppConfig.authentication field
- `crates/game-server/src/error.rs` — added AuthenticationError enum
- `crates/game-server/src/context.rs` — added AuthService to ServerContext
- `crates/game-server/src/application.rs` — added `pub fn repository()` accessor
- `migrations/0027_player_character_audit_retention.sql` — audit retention + receipt FK relaxation

Local PostgreSQL started with `docker compose up -d postgres` and reported healthy before SQLx execution.

Replace each row with the latest evidence for that area; do not append redundant historical logs.
