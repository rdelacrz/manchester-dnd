# Rewrite Part 1 — Test Results

## Baseline / latest

| Area | Command | Result |
|---|---|---|
| PostgreSQL readiness | `docker compose ps postgres` + `docker compose exec -T postgres pg_isready -U manchester_arcana -d manchester_arcana` | PASS — healthy; accepting connections |
| Full Rust workspace | `DATABASE_URL=… cargo test --locked --workspace` | PASS — 398 tests: app 7, core 170, server 211, integration 3, admin 2; all doc tests pass |
| App SSR | `cargo test --locked -p manchester-dnd-app --features ssr` | PASS — 32 lib + 6 bin tests |
| Clippy | `cargo clippy --locked --workspace --all-targets -- -D warnings` | PASS — zero warnings |
| Formatting | `cargo fmt --all -- --check` | PASS |
| Mechanic traceability | `python3 scripts/validate_mechanic_traceability.py` | PASS |

## Test growth from rewrite work

| Crate | Before | After | Delta |
|---|---|---|---|
| game-core | 169 | 170 | +1 (Task 9: campaign instance advancement independence) |
| game-server | 193 | 211 | +18 (Task 10: character-library application service + two-account isolation tests) |
| app (SSR) | 27+6 | 32+6 | +5 (Task 6: auth server function tests) |
| **Total** | **372** | **398** | **+26** |

## Commits

| Commit | Description |
|---|---|
| `ac4ba95` | Repair git damage, add auth/character-library domain, Tasks 4/9/10 |
| `5c932f0` | Wire auth boundary middleware, add comprehensive auth tests (Tasks 5/6) |

Local PostgreSQL started with `docker compose up -d postgres` and reported healthy before SQLx execution.

Replace each row with the latest evidence for that area; do not append redundant historical logs.
