#!/usr/bin/env bash

set -Eeuo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

binary="${SMOKE_BINARY:-target/release/manchester-dnd-web}"
site_root="${SMOKE_SITE_ROOT:-target/site}"
address="${SMOKE_ADDRESS:-127.0.0.1:6791}"
base_url="${SMOKE_BASE_URL:-http://$address}"
mongodb_uri="${MONGODB_URI:-mongodb://manchester_app:dev-app-password@127.0.0.1:27017/?authSource=admin&replicaSet=rs0&directConnection=true}"
mongodb_database="${MONGODB_DATABASE:-manchester_dnd}"

if [[ "$base_url" != http://* ]]; then
    echo "provider-disabled smoke: local boundary requires an http:// URL" >&2
    exit 1
fi
authority="${base_url#http://}"
authority="${authority%%/*}"
if [[ -z "$authority" ]]; then
    echo "provider-disabled smoke: could not derive Host authority from $base_url" >&2
    exit 1
fi

if [[ ! -x "$binary" ]]; then
    echo "provider-disabled smoke: build the release server first: cargo leptos build --release" >&2
    exit 1
fi
if [[ ! -d "$site_root/pkg" ]]; then
    echo "provider-disabled smoke: release site assets not found beneath $site_root/pkg" >&2
    exit 1
fi
for command in curl grep timeout; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "provider-disabled smoke: required command is unavailable: $command" >&2
        exit 1
    fi
done

rehearse_database_outage="${SMOKE_DATABASE_OUTAGE:-0}"
if [[ "$rehearse_database_outage" == "1" ]] && ! command -v docker >/dev/null 2>&1; then
    echo "provider-disabled smoke: Docker Compose is required when SMOKE_DATABASE_OUTAGE=1" >&2
    exit 1
fi

temporary_evidence=false
if [[ -n "${SMOKE_EVIDENCE_DIR:-}" ]]; then
    evidence_dir="$SMOKE_EVIDENCE_DIR"
    mkdir -p "$evidence_dir"
else
    evidence_dir="$(mktemp -d)"
    temporary_evidence=true
fi

server_pid=""
mongodb_service_stopped=false
mongodb_service="${SMOKE_MONGODB_SERVICE:-mongodb}"
if [[ ! "$mongodb_database" =~ ^[A-Za-z0-9_-]+$ ]]; then
    echo "provider-disabled smoke: MongoDB database name must be allowlisted" >&2
    exit 1
fi

cleanup() {
    if [[ "$mongodb_service_stopped" == true ]]; then
        docker compose start "$mongodb_service" >/dev/null 2>&1 || true
    fi
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
        kill -TERM "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    if [[ "$temporary_evidence" == true ]]; then
        rm -rf "$evidence_dir"
    fi
}
trap cleanup EXIT

fail() {
    echo "provider-disabled smoke: $*" >&2
    echo "provider-disabled smoke: inspect redacted evidence at $evidence_dir" >&2
    exit 1
}

env \
    APP_ENV_FILE=/dev/null \
    APP_ACCESS_MODE=local \
    LEPTOS_SITE_ADDR="$address" \
    LEPTOS_SITE_ROOT="$site_root" \
    MONGODB_URI="$mongodb_uri" \
    MONGODB_DATABASE="$mongodb_database" \
    EVENT_PROMPT_DIR="${EVENT_PROMPT_DIR:-prompts/events/private}" \
    TEXT_LLM_BACKEND=disabled \
    IMAGE_LLM_BACKEND=disabled \
    RUST_LOG="${RUST_LOG:-manchester_dnd=info,tower_http=info}" \
    "$binary" >"$evidence_dir/server.log" 2>&1 &
server_pid=$!

ready=false
for _ in $(seq 1 60); do
    if ! kill -0 "$server_pid" 2>/dev/null; then
        fail "server exited before liveness succeeded"
    fi
    if curl --silent --show-error --fail --max-time 2 "$base_url/health/live" \
        --output /dev/null 2>/dev/null; then
        ready=true
        break
    fi
    sleep 1
done
[[ "$ready" == true ]] || fail "liveness did not succeed within 60 seconds"

request() {
    local name="$1"
    shift
    curl --silent --show-error --max-time 10 \
        --dump-header "$evidence_dir/$name.headers" \
        --output "$evidence_dir/$name.body" \
        --write-out '%{http_code}' "$@"
}

status="$(request live "$base_url/health/live")" || fail "liveness request failed"
[[ "$status" == "204" ]] || fail "liveness returned HTTP $status instead of 204"

status="$(request ready "$base_url/health/ready")" || fail "readiness request failed"
[[ "$status" == "204" ]] || fail "readiness returned HTTP $status instead of 204"

status="$(request ssr "$base_url/")" || fail "SSR request failed"
[[ "$status" == "200" ]] || fail "SSR returned HTTP $status instead of 200"
grep --fixed-strings --quiet '<html lang="en">' "$evidence_dir/ssr.body" \
    || fail "SSR response did not declare English"
grep --fixed-strings --quiet '<title>Manchester Arcana</title>' "$evidence_dir/ssr.body" \
    || fail "SSR response did not contain the application title"
grep --fixed-strings --quiet 'Your city. Your stories. A realm remade.' "$evidence_dir/ssr.body" \
    || fail "SSR response did not contain the usable application shell"
grep --ignore-case --extended-regexp --quiet '^content-security-policy:.*frame-ancestors' "$evidence_dir/ssr.headers" \
    || fail "SSR response did not deny framing with Content-Security-Policy"
grep --ignore-case --extended-regexp --quiet '^x-frame-options:[[:space:]]*DENY' "$evidence_dir/ssr.headers" \
    || fail "SSR response did not include X-Frame-Options: DENY"

server_fn_path="${SMOKE_SERVER_FN_PATH:-}"
mutation_server_fn_path="${SMOKE_MUTATION_SERVER_FN_PATH:-}"
if [[ -z "$server_fn_path" || -z "$mutation_server_fn_path" ]]; then
    shopt -s nullglob
    wasm_files=("$site_root"/pkg/*.wasm)
    shopt -u nullglob
    for wasm_file in "${wasm_files[@]}"; do
        if [[ -z "$server_fn_path" ]]; then
            server_fn_path="$(LC_ALL=C grep --binary-files=text --only-matching --max-count=1 \
                --extended-regexp '/api/load_local_campaign[0-9]+' "$wasm_file" || true)"
        fi
        if [[ -z "$mutation_server_fn_path" ]]; then
            mutation_server_fn_path="$(LC_ALL=C grep --binary-files=text --only-matching --max-count=1 \
                --extended-regexp '/api/attempt_exploration_check[0-9]+' "$wasm_file" || true)"
        fi
        [[ -n "$server_fn_path" && -n "$mutation_server_fn_path" ]] && break
    done
fi
[[ -n "$server_fn_path" ]] || fail "could not discover the load-local-campaign server-function path"
[[ -n "$mutation_server_fn_path" ]] \
    || fail "could not discover a typed mutation server-function path"

server_fn_request() {
    local name="$1"
    local host="$2"
    local origin="$3"
    request "$name" \
        --request POST \
        --header "Host: $host" \
        --header "Origin: $origin" \
        --header 'Accept: application/json' \
        --header 'Content-Type: application/x-www-form-urlencoded' \
        --data '' \
        "$base_url$server_fn_path"
}

status="$(server_fn_request server-fn-valid "$authority" "$base_url")" \
    || fail "valid server-function request failed"
[[ "$status" == "200" ]] || fail "valid server-function request returned HTTP $status"
grep --fixed-strings --quiet '"status":"ready"' "$evidence_dir/server-fn-valid.body" \
    || fail "provider-disabled non-AI campaign load did not complete"

status="$(request server-fn-malformed \
    --request POST \
    --header "Host: $authority" \
    --header "Origin: $base_url" \
    --header 'Accept: application/json' \
    --header 'Content-Type: application/x-www-form-urlencoded' \
    --data 'unexpected_mechanic=20' \
    "$base_url$mutation_server_fn_path")" || fail "malformed server-function request failed"
[[ "$status" == "400" ]] || fail "malformed server input returned HTTP $status instead of 400"
grep --fixed-strings --quiet '"code":"invalid_server_input"' "$evidence_dir/server-fn-malformed.body" \
    || fail "malformed server input did not return the stable redacted code"
if grep --ignore-case --extended-regexp --quiet 'Args\||decoder|unexpected_mechanic|serverfnerror' \
    "$evidence_dir/server-fn-malformed.body" "$evidence_dir/server-fn-malformed.headers"; then
    fail "malformed server input exposed transport decoder details"
fi

status="$(server_fn_request server-fn-forged-origin "$authority" 'https://malicious.example')" \
    || fail "forged-Origin request failed at the transport layer"
[[ "$status" == "200" ]] || fail "forged-Origin request returned unexpected HTTP $status"
grep --fixed-strings --quiet '"code":"invalid_request_origin"' "$evidence_dir/server-fn-forged-origin.body" \
    || fail "forged Origin was not rejected with invalid_request_origin"

status="$(server_fn_request server-fn-forged-host 'malicious.example' 'http://malicious.example')" \
    || fail "forged-Host request failed at the transport layer"
[[ "$status" == "421" ]] || fail "forged-Host request returned HTTP $status instead of 421"
grep --fixed-strings --quiet '"code":"invalid_request_host"' "$evidence_dir/server-fn-forged-host.body" \
    || fail "forged Host was not rejected with invalid_request_host"

set +e
timeout 10 env \
    APP_ENV_FILE=/dev/null \
    APP_ACCESS_MODE=hosted \
    LEPTOS_SITE_ADDR="$address" \
    LEPTOS_SITE_ROOT="$site_root" \
    MONGODB_URI="$mongodb_uri" \
    MONGODB_DATABASE="$mongodb_database" \
    TEXT_LLM_BACKEND=disabled \
    IMAGE_LLM_BACKEND=disabled \
    "$binary" >"$evidence_dir/hosted-startup.log" 2>&1
hosted_status=$?
set -e
if [[ $hosted_status -eq 0 || $hosted_status -eq 124 ]]; then
    fail "hosted mode did not fail closed during startup"
fi
grep --fixed-strings --quiet 'APP_ACCESS_MODE' "$evidence_dir/hosted-startup.log" \
    || fail "hosted-mode startup failure was not field-specific"

set +e
timeout 10 env \
    APP_ENV_FILE=/dev/null \
    APP_ACCESS_MODE=local \
    LEPTOS_SITE_ADDR="$address" \
    LEPTOS_SITE_ROOT="$site_root" \
    MONGODB_URI="$mongodb_uri" \
    MONGODB_DATABASE="$mongodb_database" \
    TEXT_LLM_BACKEND=not-a-provider \
    IMAGE_LLM_BACKEND=disabled \
    "$binary" >"$evidence_dir/invalid-config.log" 2>&1
invalid_status=$?
set -e
if [[ $invalid_status -eq 0 || $invalid_status -eq 124 ]]; then
    fail "invalid provider configuration did not fail during startup"
fi
grep --fixed-strings --quiet 'TEXT_LLM_BACKEND' "$evidence_dir/invalid-config.log" \
    || fail "invalid configuration failure was not field-specific"

if [[ "$rehearse_database_outage" == "1" ]]; then
    docker compose stop "$mongodb_service" >/dev/null
    mongodb_service_stopped=true

    status="$(request database-outage-live "$base_url/health/live")" \
        || fail "liveness failed during database outage"
    [[ "$status" == "204" ]] || fail "liveness returned HTTP $status during database outage"

    database_unready=false
    for _ in $(seq 1 20); do
        status="$(request database-outage-ready "$base_url/health/ready")" \
            || fail "readiness request failed during database outage"
        if [[ "$status" == "503" ]]; then
            database_unready=true
            break
        fi
        sleep 1
    done
    [[ "$database_unready" == true ]] \
        || fail "readiness did not become 503 during the controlled database outage"

    docker compose start "$mongodb_service" >/dev/null
    mongodb_service_stopped=false

    database_recovered=false
    for _ in $(seq 1 30); do
        status="$(request database-recovered-ready "$base_url/health/ready")" \
            || fail "readiness request failed after database recovery"
        if [[ "$status" == "204" ]]; then
            database_recovered=true
            break
        fi
        sleep 1
    done
    [[ "$database_recovered" == true ]] \
        || fail "readiness did not recover after database connections were restored"
fi

echo "provider-disabled smoke: liveness, readiness, SSR, non-AI campaign load, input/Host/Origin rejection, startup fail-closed, and requested outage checks passed"
if [[ "$temporary_evidence" == false ]]; then
    echo "provider-disabled smoke: evidence retained at $evidence_dir"
fi
