# Slice 0 local operator runbook

This runbook operates Manchester Arcana's current **local single-user** deployment.
Hosted or reverse-proxied operation is unavailable: `APP_ACCESS_MODE=hosted` fails
startup until authenticated sessions, CSRF protection, and campaign authorization
land in Slice 4. Local Host/Origin checks reduce browser cross-origin attacks; they
are not authentication and do not protect the game from another process running as
the same user.

## Supported runtime shape

| Listener | Default | Exposure | Purpose |
| --- | --- | --- | --- |
| Application HTTP | `127.0.0.1:6789` | Loopback only | SSR, WASM/static assets, server functions, probes |
| PostgreSQL | `127.0.0.1:5432` | Loopback only in `compose.yaml` | Authoritative campaigns, characters, audits, receipts, generated-asset metadata |
| Cargo Leptos reload | `127.0.0.1:7000` | Development only | Hot-reload websocket |

Do not change the application bind to `0.0.0.0`, a LAN address, or a public
interface. Local mode rejects that configuration. Do not add a reverse proxy to
work around the guard.

## Runtime configuration

Use an environment file readable only by the operator (`chmod 600 .env`) or a
secret manager. Environment values override dotenv values. `APP_ENV_FILE` selects
an explicit dotenv file; an empty value is invalid.

| Variable | Required operational value | Secret | Notes |
| --- | --- | --- | --- |
| `APP_ACCESS_MODE` | `local` | No | `hosted` deliberately fails closed. |
| `LEPTOS_SITE_ADDR` | Loopback socket, normally `127.0.0.1:6789` | No | Cargo metadata supplies the default. |
| `LEPTOS_SITE_ROOT` | Built site, normally `target/site` or `/app/site` | No | Must contain `pkg/manchester-arcana.wasm` and its JS/CSS. |
| `DATABASE_URL` | PostgreSQL URL for the application role | **Yes** | Use TLS whenever traffic leaves the host/private namespace. The app applies embedded migrations at startup. |
| `DATABASE_MIGRATE_ON_START` | `true` for local development; `false` with separated migration/app roles | No | The `database-migrate` operator binary runs embedded migrations first. |
| `DATABASE_MAX_CONNECTIONS` | `1`–`50`; default `10` | No | Budget across every web/worker process plus operator reserve. |
| `DATABASE_ACQUIRE_TIMEOUT_MILLISECONDS` | `1`–`60000`; default `5000` | No | Bounds pool waits. |
| `DATABASE_STATEMENT_TIMEOUT_MILLISECONDS` | `1`–`300000`; default `30000` | No | Applied to every pool connection. |
| `DATABASE_LOCK_TIMEOUT_MILLISECONDS` | `1`–`60000`; default `5000` | No | Prevents indefinite row-lock waits. |
| `DATABASE_IDLE_TRANSACTION_TIMEOUT_MILLISECONDS` | `1`–`300000`; default `15000` | No | Terminates abandoned open transactions. |
| `CONTENT_PACK_ROOT` | Read-only bundled pack root | No | Default `content/packs` (container: `/app/content/packs`). Startup validates only the compiled `core-mvp`, `rainbound-borough`, and `emberline-archive` directories and rejects missing, quarantined, or non-matching exact pins. |
| `CONTENT_DEFAULT_THEME_PACK_ID` | Bundled theme pack ID | No | Must be `dev.manchester-arcana.rainbound-borough` or `dev.manchester-arcana.emberline-archive`; default is Rainbound Borough. |
| `EVENT_PROMPT_DIR` | Bounded local event directory | Potentially sensitive | Default `prompts/events/private`; mount read-only in a container. |
| `INSPIRATION_ENABLED` | Deployment-wide private-inspiration gate | No | Defaults to `false`; a campaign opt-in can only narrow this gate. |
| `RNG_MASTER_KEY_FILE` | Protected writable path, normally `data/rng-master.key` | **Yes** | Created as 32 random bytes with mode `0600`; back it up atomically with PostgreSQL or exact roll replay will be lost. Never place it in the static site. |
| `IMAGE_ARTIFACT_ROOT` | Protected non-public path, normally `data/generated-images` | Potentially sensitive | Validated scene-image/quarantine bytes; public/build-output paths are rejected. |
| `RUST_LOG` | Bounded tracing filter | No | Do not enable payload/prompt logging. |
| `TEXT_LLM_BACKEND` | `disabled` for Slice 0/local smoke | No | `openai-compatible` requires an approved URL and model. |
| `TEXT_LLM_BASE_URL` | HTTPS provider root when enabled | No | Plain HTTP is accepted only for loopback providers. Credentials, query strings, and fragments are forbidden. |
| `TEXT_LLM_API_KEY` | Provider credential when required | **Yes** | Redacted by configuration types; never put it in source or a command line. |
| `TEXT_LLM_MODEL` | Provider model when enabled | No | Maximum 256 characters. |
| `TEXT_LLM_TIMEOUT_SECONDS` | `1`–`600` | No | Default `60`. |
| `TEXT_LLM_TEMPERATURE` | finite `0`–`2` | No | Default `0.7` in the example profile. |
| `TEXT_LLM_MAX_OUTPUT_TOKENS` | `1`–`128000` | No | Example value `1400`. |
| `TEXT_LLM_ESTIMATED_REQUEST_COST_MICROUSD` | Operator estimate per request | No | Required for `openai-compatible`; measured in millionths of one US dollar. |
| `IMAGE_LLM_BACKEND` | `disabled` for Slice 0/local smoke | No | Separate from the text provider. |
| `IMAGE_LLM_BASE_URL` | HTTPS provider root when enabled | No | Same URL boundary as text. |
| `IMAGE_LLM_API_KEY` | Provider credential when required | **Yes** | Never expose it to browser assets or logs. |
| `IMAGE_LLM_MODEL` | Provider model when enabled | No | Maximum 256 characters. |
| `IMAGE_LLM_TIMEOUT_SECONDS` | `1`–`600` | No | Example value `90`. |
| `IMAGE_LLM_SIZE` | Bounded provider size identifier | No | Alphanumeric plus `x`, `-`, and `_`; maximum 64 characters. |
| `IMAGE_LLM_ESTIMATED_REQUEST_COST_MICROUSD` | Operator estimate per request | No | Required for `openai-compatible`; measured in millionths of one US dollar. |
| `GENERATION_CAMPAIGN_*_BUDGET` | Lifetime request/token/latency/cost ceilings | No | Defaults are in `.env.example`; a zero cost ceiling blocks requests with non-zero estimated cost. |
| `GENERATION_TURN_*_BUDGET` | Per-turn request/token/latency/cost ceilings | No | Each value must not exceed its corresponding campaign ceiling. |
| `GENERATION_MAX_CAMPAIGN_CONCURRENCY` | `1`–`32` | No | Transactional provider-work reservation cap; default `2`. |
| `GENERATION_WORKER_BATCH_SIZE` | `1`–`100` | No | Bounded recovery/cleanup worker batch; default `4`. |

Invalid configuration exits before the listener starts and identifies the field,
without including credential values. The provider-disabled smoke script exercises
invalid `TEXT_LLM_BACKEND`, hosted-mode rejection, and the local browser boundary.

## Source-checkout start and stop

The application declares Rust 1.88 as its minimum version. Building the pinned
`cargo-leptos` 0.3.7 tool from its published lockfile currently requires Rust 1.90,
so the container and CI use Rust 1.90.0. Other prerequisites are the
`wasm32-unknown-unknown` target, Docker Compose, `curl`, and the PostgreSQL client
when running the controlled outage rehearsal.

```sh
rustup toolchain install 1.90.0 --profile minimal --target wasm32-unknown-unknown
export RUSTUP_TOOLCHAIN=1.90.0
cargo install cargo-leptos --locked --version 0.3.7
docker compose up -d --wait postgres
cp .env.example .env
chmod 600 .env
cargo leptos watch
```

Open <http://127.0.0.1:6789>. Stop the foreground application with `Ctrl+C`, then
stop PostgreSQL without deleting its volume:

```sh
docker compose stop postgres
```

`docker compose down` also preserves the named volume. **Do not run**
`docker compose down -v` unless deletion is intentional and a verified logical
backup plus restore rehearsal exists.

For a release-mode local run:

```sh
cargo leptos build --release --bin-cargo-args=--locked --lib-cargo-args=--locked
APP_ACCESS_MODE=local \
TEXT_LLM_BACKEND=disabled \
IMAGE_LLM_BACKEND=disabled \
target/release/manchester-dnd-web
```

Stop a background release process with `kill -TERM PID`, wait for it to exit, and
only then stop PostgreSQL. The current server does not advertise a drain window;
avoid stopping it during an in-flight command.

## Container build and local execution

The multi-stage image is a non-root, provider-disabled runtime artifact:

```sh
docker build --pull -t manchester-arcana:local .
```

Local mode binds to loopback **inside its network namespace**. Ordinary
`docker run -p 6789:6789` therefore cannot provide a supported boundary. On Linux,
use host networking so container loopback is host loopback:

```sh
docker run --rm --network host \
  --env APP_ACCESS_MODE=local \
  --env DATABASE_URL \
  --env TEXT_LLM_BACKEND=disabled \
  --env IMAGE_LLM_BACKEND=disabled \
  --mount type=volume,src=manchester-arcana-rng,dst=/app/data \
  --mount type=bind,src="$PWD/prompts/events/private",dst=/app/prompts/events/private,readonly \
  manchester-arcana:local
```

Pass `DATABASE_URL` through the environment or a secret facility, not as a Docker
build argument. Retain and encrypt the `manchester-arcana-rng` volume with the
database backups; replacing it changes future replay streams. Docker
Desktop/bridged/container-orchestrated serving is not a
supported deployment until hosted mode exists. `EXPOSE 6789` documents the
process port; it does not authorize publishing it.

## Health and smoke checks

```sh
curl --fail --include http://127.0.0.1:6789/health/live
curl --fail --include http://127.0.0.1:6789/health/ready
```

- `/health/live` returns `204` when the HTTP process can answer.
- `/health/ready` returns `204` when `SELECT 1` succeeds and `503` when a running
  process loses database connectivity.
- Readiness does **not** check backup freshness, free disk, event-pack readability,
  provider health, or provider budget.
- Initial database connection and migrations occur during startup. A database that
  is already unavailable can prevent the listener from starting, so liveness is not
  expected in that case.
- Required content packs are validated against compiled ruleset, capability, and
  exact digest pins before the database connection. Invalid or missing bundled
  content prevents the listener from starting; a running process retains an
  immutable in-memory catalog.

Build and exercise the exact provider-disabled release boundary with:

```sh
cargo leptos build --release --bin-cargo-args=--locked --lib-cargo-args=--locked
DATABASE_URL=postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/manchester_arcana \
SMOKE_DATABASE_OUTAGE=1 \
scripts/provider-disabled-smoke.sh
```

The script starts its own process on `127.0.0.1:6791`, checks liveness, readiness,
SSR and anti-framing headers, completes the non-AI campaign-load path, rejects
malformed typed input with a redacted stable code, proves forged Host/Origin
requests fail safely, and proves hosted and invalid-provider startup fail closed.
With `SMOKE_DATABASE_OUTAGE=1`, the application role must be allowed to administer
the disposable smoke database: the script temporarily disables new connections,
terminates existing database sessions, proves `live=204` and `ready=503`, restores
connections in its exit trap, and waits for `ready=204` before browser tests. Never
run that rehearsal against a shared or production database. Set
`SMOKE_EVIDENCE_DIR` to retain response headers, bodies, and logs for canary
scanning.

The HTTP boundary rejects a non-loopback or malformed `Host` with `421` and the safe
`invalid_request_host` code before routing. Gameplay server functions additionally
require a matching loopback HTTP `Origin`; a mismatch returns the typed
`invalid_request_origin` result without executing the command.

## Data location and ownership

PostgreSQL is the authoritative structured database. With the default Compose project,
its files live in the Docker named volume `manchester-dnd_postgres-data` (confirm
with `docker volume ls` and `docker volume inspect`). The repository `data/`
directory is not an application database; it contains the protected RNG master key
unless `RNG_MASTER_KEY_FILE` points elsewhere. Browser storage is presentation-only
and must never become authoritative game state.

Keep private prompt/event files outside version control and mount them read-only.
Validated scene-image bytes live beneath `IMAGE_ARTIFACT_ROOT` with mode `0600`;
PostgreSQL stores bounded metadata, protected relative keys, and digests. Neither
location is a public/static root.

## Backup and recovery

The complete, tested procedure—including encrypted chunked vaults, 30-day expiry,
separated roles, TLS/pool/lock policy, monitoring, isolated restore, artifact/key
verification, migration/read-only response, and the retained legacy SQLite
import—is in [PostgreSQL reliability and recovery](database-recovery.md).

For the supported Compose development profile, stop mutating processes and run:

```sh
scripts/run-database-recovery-drill.sh
```

The script creates a PostgreSQL custom-format dump, bundles the RNG key and
protected artifacts, authenticates/encrypts it under an operator-only key,
restores into a uniquely named separate database, compares deterministic state
manifests, exercises exact expiry, records bounded evidence, and drops the test
database. It never restores over the source.

### Manual fallback

Prefer a PostgreSQL custom-format logical dump. It is portable and supports a
restore into a separate verification database.

Copy the exact RNG master key into the same encrypted recovery set under a private
name and verify its digest without printing its contents. A database restored with
a different key remains readable but cannot reproduce protected historical random
streams, so the database dump and key are one recovery unit.

```sh
install -d -m 700 backups
backup="backups/manchester_arcana-$(date -u +%Y%m%dT%H%M%SZ).dump"
umask 077
docker compose exec -T postgres \
  pg_dump -U manchester_arcana -d manchester_arcana \
  --format=custom --no-owner --no-acl > "$backup"
sha256sum "$backup" > "$backup.sha256"
docker compose exec -T postgres pg_restore --list < "$backup" > /dev/null
```

A zero exit code and readable archive are necessary but not sufficient. Periodically
perform the separate-database restore below and record the date, PostgreSQL version,
application build, checksum, and gameplay verification. Copy backups off the
application host according to the retention decision; the named volume is not a
backup.

## Restore and safe recovery

Never restore over the only copy of a database. Use a new database and keep the
application stopped while choosing the recovery point.

```sh
backup=backups/manchester_arcana-YYYYMMDDTHHMMSSZ.dump
sha256sum --check "$backup.sha256"

# Stop only the application. Keep PostgreSQL running.
restore_db="manchester_arcana_restore_$(date -u +%Y%m%d%H%M%S)"
docker compose exec -T postgres \
  createdb -U manchester_arcana "$restore_db"
docker compose exec -T postgres \
  pg_restore -U manchester_arcana -d "$restore_db" \
  --exit-on-error --no-owner --no-acl < "$backup"
```

Start the exact release build against the restored database. Startup applies only
new, embedded migrations:

```sh
DATABASE_URL="postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/$restore_db" \
APP_ACCESS_MODE=local \
TEXT_LLM_BACKEND=disabled \
IMAGE_LLM_BACKEND=disabled \
target/release/manchester-dnd-web
```

Verify both probes, load the saved campaign, compare its revision/latest roll to the
recovery record, and complete one disposable non-AI action only if the recovery
procedure permits mutation. Then stop the verification process and switch the
operator-managed `DATABASE_URL` to the restored database. Keep the old database and
backup until the recovery is signed off.

If a migration fails, stop. Do not edit an already distributed migration and do not
drop the original database. Preserve redacted startup diagnostics, fix forward with
a new migration, or restore the last known-good backup into another new database.
If corruption or unexpected revision drift is suspected, make a fresh logical dump
before attempting repair and retain the immutable turn audit.
