# Local and release operations

This runbook covers the supported local/Compose profile and the fail-closed hosted
profile. MongoDB is authoritative. DragonflyDB is optional cache/pub-sub state.

## Network boundary

| Service | Default bind | Exposure | Purpose |
| --- | --- | --- | --- |
| Application HTTP | `127.0.0.1:6789` | Loopback in local mode | SSR, WASM/static assets, server functions, probes |
| MongoDB replica set | `127.0.0.1:27017` | Loopback development only | Authoritative accounts, campaigns, gameplay, jobs, audits, metadata |
| DragonflyDB | `127.0.0.1:6379` | Loopback development only | Disposable cache, invalidation, pub/sub |
| Cargo Leptos reload | `127.0.0.1:7000` | Development only | Hot-reload websocket |

Do not expose the local profile through a reverse proxy. Hosted mode requires an
HTTPS canonical origin, secure cookies, authenticated sessions, MongoDB TLS, and
secret-managed cryptographic keys.

## Important configuration

| Variable | Meaning | Secret |
| --- | --- | --- |
| `APP_ACCESS_MODE` | `local` or `hosted` | No |
| `LEPTOS_SITE_ADDR` | Application bind address | No |
| `LEPTOS_SITE_ROOT` | Built site directory | No |
| `MONGODB_URI` | Least-privilege application URI | **Yes** |
| `MONGODB_SCHEMA_URI` | Schema-admin URI used only by `mongo-admin` | **Yes** |
| `MONGODB_DATABASE` | Managed application database | No |
| `DRAGONFLY_URL` | Optional authenticated cache/pub-sub URL | **Yes** |
| `AUTH_COOKIE_SECURE` | Must be `true` in hosted mode | No |
| `AUTH_PUBLIC_ORIGIN` | Canonical HTTPS origin in hosted mode | No |
| `AUTH_EMAIL_LOOKUP_KEY` | Email lookup HMAC key | **Yes** |
| `AUTH_DATA_ENCRYPTION_KEY` | Envelope encryption key | **Yes** |
| `RNG_MASTER_KEY_FILE` | Protected server-owned randomness key | **Yes** |
| `IMAGE_ARTIFACT_ROOT` | Non-public generated image/quarantine root | Sensitive |
| `TEXT_LLM_BACKEND` | `disabled`, `fake`, or approved provider | Provider token may be secret |
| `IMAGE_LLM_BACKEND` | `disabled`, `fake`, or approved provider | Provider token may be secret |
| `INSPIRATION_ENABLED` | Deployment-wide private-inspiration gate | No |
| `EVENT_PROMPT_DIR` | Offline inspiration-admin input only | Sensitive |

Do not put secrets in build arguments, images, logs, command lines, browser assets,
or checked-in files. Local defaults in `.env.example`/`compose.yaml` are not
production credentials.

## Start local development

```sh
rustup toolchain install 1.90.0 --profile minimal \
  --component rustfmt,clippy --target wasm32-unknown-unknown
cargo install --locked --version 0.3.7 cargo-leptos

docker compose up -d --wait mongodb dragonfly
cargo run --locked -p manchester-dnd-server --bin mongo-admin -- schema apply
cargo run --locked -p manchester-dnd-server --bin mongo-admin -- schema verify
cargo leptos watch
```

Open <http://127.0.0.1:6789>. Keep text/image providers and private inspiration
disabled unless their approval and consent prerequisites are met.

Stop the app first, then services without deleting volumes:

```sh
docker compose stop dragonfly mongodb
```

## Accounts and signup access

Signup requires a one-use access token. Issue it from the trusted operator shell;
never transmit it in logs or screenshots:

```sh
cargo run --locked -p manchester-dnd-server --bin issue-signup-token -- user
```

Hosted mode requires explicit secure-cookie/origin/key configuration and rejects
loopback/plaintext database settings.

## Release build

```sh
scripts/check-release-dependencies.sh
cargo leptos build --release \
  --bin-cargo-args=--locked --lib-cargo-args=--locked
```

Before starting the app, reconcile schema with the schema-admin URI, then remove
that URI from the application process environment:

```sh
MONGODB_URI="$APP_MONGODB_URI" \
MONGODB_SCHEMA_URI="$SCHEMA_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
  target/release/mongo-admin schema apply

MONGODB_URI="$APP_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
APP_ACCESS_MODE=local \
TEXT_LLM_BACKEND=disabled \
IMAGE_LLM_BACKEND=disabled \
  target/release/manchester-dnd-web
```

## Health and degraded operation

- `GET /health/live` proves the process/event loop is responsive.
- `GET /health/ready` proves authoritative MongoDB access and schema compatibility.
- DragonflyDB failure is reported separately and does not make authoritative
  MongoDB reads/writes unsafe. Cache failures must become misses/no-op publication.
- Text/image provider failure uses deterministic fallback or a bounded unavailable
  response; it must not corrupt game state.

Run the disabled-provider and optional Mongo outage smoke test:

```sh
MONGODB_URI="$LOCAL_APP_MONGODB_URI" \
MONGODB_DATABASE=manchester_dnd \
SMOKE_DATABASE_OUTAGE=1 \
  scripts/provider-disabled-smoke.sh
```

The outage phase stops/restarts only the Compose MongoDB service and expects liveness
to remain 200 while readiness fails closed.

## Container smoke

```sh
scripts/run-container-smoke.sh target/release/manchester-dnd-web
```

The helper container uses host networking, read-only root filesystem, no Linux
capabilities, a non-root UID, bounded memory/PIDs, and an isolated writable volume
for protected runtime state. It starts no provider calls and passes MongoDB/
Dragonfly settings through environment secrets.

## Tests

Live repository/application tests use randomized `mdnd_test_*` MongoDB databases and
apply the managed schema independently. They refuse unsafe database names and drop
only their own database.

```sh
MONGODB_TEST_URI="$LOOPBACK_TEST_ADMIN_URI" \
  cargo test --locked --workspace -- --test-threads=1

cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
npm run test:browser
```

Do not point `MONGODB_TEST_URI` at production.

## Data location and ownership

MongoDB is the system of record. The default Compose project stores it in the
`mongodb-data` named volume. DragonflyDB data is disposable and may be flushed.
Protected RNG/image/source/recovery files live outside the static site and should be
mode `0600`/operator-owned where appropriate.

Never place generated/private artifacts under `target/site`, `app/`, or another
HTTP-served root. MongoDB stores bounded metadata, protected relative keys, and
digests—not raw private inspiration Markdown.

## Backup and restore

Use [MongoDB reliability and recovery](database-recovery.md). For the supported
Compose profile:

```sh
MONGODB_TEST_URI="$LOOPBACK_TEST_ADMIN_URI" \
MONGODB_DATABASE=manchester_dnd \
  scripts/run-database-recovery-drill.sh
```

The drill creates a compressed MongoDB archive, bundles protected RNG/image state,
authenticates/encrypts it, opens it, restores into a separate random database,
verifies managed validators/indexes and collection counts, then drops the restore
database. DragonflyDB is explicitly excluded.

## Incident response

1. Stop optional generation/inspiration workers before invasive diagnosis.
2. Preserve redacted correlation IDs, health output, `database-ops` output, and disk
   capacity. Never capture prompt/source/account/campaign bodies unnecessarily.
3. On low disk, stop mutation; never delete MongoDB journal/data files, selected
   artifacts, RNG keys, or the only backup.
4. On schema/validation errors, preserve the source and restore into an isolated
   database; fix forward through the managed schema catalog.
5. On Dragonfly failure/loss, restart or flush it and allow reconstruction from
   MongoDB. Never restore cache data as authority.
6. Resume only after schema verification, readiness, an idempotent read, and a fresh
   recovery drill pass.
