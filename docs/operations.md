# Operations

## Build and run

Install Rust 1.90 with `wasm32-unknown-unknown`, `cargo-leptos` 0.3.7, and Docker
Compose. Start the MongoDB replica set and optional Dragonfly cache:

```sh
docker compose up -d --wait mongodb dragonfly
cp .env.example .env
cargo run --locked -p manchester-dnd-server --bin mongo-admin -- schema apply
cargo leptos watch
```

Open <http://127.0.0.1:6789>. Keep providers and private inspiration disabled unless
their documented gates are satisfied.

## Configuration

- `APP_ACCESS_MODE`: `local` or fail-closed authenticated `hosted`.
- `MONGODB_URI`: secret-bearing least-privilege application URI.
- `MONGODB_SCHEMA_URI`: schema-admin URI used only by `mongo-admin`.
- `MONGODB_DATABASE`: managed application database.
- `DRAGONFLY_URL`: optional authenticated cache/pub-sub URI.
- `AUTH_*`: hosted cookie/origin/HMAC/encryption controls.
- `RNG_MASTER_KEY_FILE`, `IMAGE_ARTIFACT_ROOT`: protected non-public state.
- `TEXT_LLM_*`, `IMAGE_LLM_*`: independent generation profiles.
- `INSPIRATION_ENABLED`, `EVENT_PROMPT_DIR`: default-off offline-source boundary.

MongoDB stores authoritative state. Dragonfly is disposable. Protected image/RNG/
source/recovery files stay outside static roots.

## Health and recovery

`GET /health/live` reports process liveness. `GET /health/ready` verifies MongoDB and
managed schema; Dragonfly/provider health is separate and may degrade safely.

Before recovery, stop writes and preserve the source database. Restore an encrypted
Mongo archive into a separate random database, verify all validators/indexes and
state counts/manifests, then exercise exact load/resume before cutover:

```sh
MONGODB_TEST_URI="$LOOPBACK_TEST_ADMIN_URI" \
MONGODB_DATABASE=manchester_dnd \
  scripts/run-database-recovery-drill.sh
```

See [MongoDB reliability and recovery](operations/database-recovery.md) and the
full [local/release runbook](operations/slice-0-runbook.md).

## Provider/cache degraded smoke

```sh
MONGODB_URI="$LOCAL_APP_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
SMOKE_DATABASE_OUTAGE=1 \
  scripts/provider-disabled-smoke.sh
```

Provider failure must use deterministic fallback. Dragonfly failure must become a
cache miss/no-op publication. MongoDB loss keeps liveness up but readiness and
authoritative operations fail closed.
