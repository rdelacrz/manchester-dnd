# Operator guide

Manchester Arcana is currently a local-only application. Hosted mode intentionally fails closed until authenticated sessions and campaign authorization are implemented.

## Build and run

Install stable Rust with the `wasm32-unknown-unknown` target, `cargo-leptos`, and PostgreSQL 17. For development, start the database and application from the repository root:

```sh
docker compose up -d --wait postgres
cp .env.example .env
cargo leptos watch
```

For a release build and process:

```sh
cargo leptos build --release
cargo leptos serve --release
```

The HTTP listener defaults to `127.0.0.1:6789`; the development reload server uses port `7000`. Local mode rejects non-loopback binding. Do not expose either port through a reverse proxy or public interface.

## Runtime configuration

`APP_ENV_FILE` selects a dotenv file. Existing process environment variables take precedence. The supported variables and safe development defaults are documented in [`.env.example`](../.env.example).

- `APP_ACCESS_MODE`: `local` is the only deployable mode today; `hosted` fails startup.
- `DATABASE_URL`: PostgreSQL connection URL. Production-like deployments must use secret-managed credentials and encrypted transport when traffic leaves a trusted host or private network.
- `EVENT_PROMPT_DIR`: directory for private event Markdown; defaults to `prompts/events/private`.
- `RUST_LOG`: tracing filter. Prompts, credentials, source Markdown, and generated binary bodies must not be enabled as log fields.
- `TEXT_LLM_*` and `IMAGE_LLM_*`: independent generation profiles. Keep both backends `disabled` unless generation is deliberately configured.

The application has no authoritative filesystem data directory. PostgreSQL stores campaign state. Private event prompts live under `EVENT_PROMPT_DIR`; generated asset persistence is not live yet. The repository's `data/` directory is reserved and must not be treated as a database backup.

## Health and recovery

`GET /health/live` reports that the process can serve HTTP. `GET /health/ready` separately checks PostgreSQL and returns `503` when the database is unavailable. Readiness does not yet verify backups, disk capacity, or provider health.

Before recovery, stop writes and preserve the failed database volume. Restore PostgreSQL from a verified backup into a separate database, run migrations with `cargo sqlx migrate run`, and validate `/health/ready` plus an exact campaign reload before directing the application to it. Never delete or overwrite the only copy of a database or legacy embedded-database file. The legacy migration constraints are described in the [persistence plan](planning/05-persistence.md).

The provider-disabled deployment check can be run against the development database with:

```sh
./scripts/smoke-provider-disabled.sh
```

