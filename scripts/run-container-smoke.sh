#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

IMAGE=${1:?usage: run-container-smoke.sh IMAGE [OUTPUT_DIR]}
OUTPUT_DIR=${2:-target/release-evidence/container-smoke}
PORT=${CONTAINER_SMOKE_PORT:-6798}
MONGODB_URI=${CONTAINER_SMOKE_MONGODB_URI:-mongodb://manchester_app:dev-app-password@127.0.0.1:27017/?authSource=admin&replicaSet=rs0&directConnection=true}
MONGODB_DATABASE=${CONTAINER_SMOKE_MONGODB_DATABASE:-manchester_dnd}
HELPER_IMAGE=mongo:8.0.26-noble@sha256:ffa440e8d62533e24a67696ae1bbb46e610ebb3167d65abd122b496ae06d28e6
suffix="${BASHPID:-$$}"
container_name="manchester-arcana-smoke-${suffix}"
volume_name="manchester-arcana-smoke-data-${suffix}"
work_dir=$(mktemp -d)

cleanup() {
  docker container rm --force "$container_name" >/dev/null 2>&1 || true
  docker volume rm --force "$volume_name" >/dev/null 2>&1 || true
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

fail() {
  echo "container smoke failed: $*" >&2
  docker container inspect "$container_name" \
    --format 'state={{.State.Status}} exit={{.State.ExitCode}} error={{.State.Error}}' \
    >&2 2>/dev/null || true
  docker container logs "$container_name" >&2 2>/dev/null || true
  exit 1
}

install -d -m 0755 "$OUTPUT_DIR"
head -c 32 /dev/urandom >"$work_dir/rng-master.key"
chmod 0600 "$work_dir/rng-master.key"
text_canary="MA_TEXT_$(openssl rand -hex 24)"
image_canary="MA_IMAGE_$(openssl rand -hex 24)"

docker volume create "$volume_name" >/dev/null
docker run --rm --interactive --user 0:0 \
  --volume "$volume_name:/data" \
  "$HELPER_IMAGE" sh -c \
  'umask 077; cat > /data/rng-master.key; chown -R 65532:65532 /data; chmod 0700 /data' \
  <"$work_dir/rng-master.key"

docker run --detach \
  --name "$container_name" \
  --network host \
  --volume "$volume_name:/app/data" \
  --env "MONGODB_URI=$MONGODB_URI" \
  --env "MONGODB_DATABASE=$MONGODB_DATABASE" \
  --env "LEPTOS_SITE_ADDR=127.0.0.1:${PORT}" \
  --env RNG_MASTER_KEY_FILE=/app/data/rng-master.key \
  --env "TEXT_LLM_API_KEY=$text_canary" \
  --env "IMAGE_LLM_API_KEY=$image_canary" \
  "$IMAGE" >/dev/null

network_request() {
  local path=$1
  local request_host=${2:-127.0.0.1:${PORT}}
  docker run --rm \
    --network "container:$container_name" \
    "$HELPER_IMAGE" bash -eu -c \
    'exec 3<>"/dev/tcp/127.0.0.1/$3"; printf "GET %s HTTP/1.1\r\nHost: %s\r\nConnection: close\r\n\r\n" "$1" "$2" >&3; cat <&3' \
    sh "$path" "$request_host" "$PORT"
}

wait_until_ready() {
  for _ in $(seq 1 60); do
    if ! docker container inspect "$container_name" --format '{{.State.Running}}' \
      | grep --fixed-strings --quiet true; then
      fail "container exited before readiness"
    fi
    if network_request /health/ready >"$work_dir/ready.raw" 2>/dev/null \
      && tr -d '\r' <"$work_dir/ready.raw" | head -n 1 \
        | grep --fixed-strings --quiet 'HTTP/1.1 204'; then
      return 0
    fi
    sleep 1
  done
  fail "readiness did not succeed within 60 seconds"
}
wait_until_ready

request() {
  local name=$1
  local path=$2
  local request_host=${3:-127.0.0.1:${PORT}}
  network_request "$path" "$request_host" >"$work_dir/$name.raw"
  tr -d '\r' <"$work_dir/$name.raw" >"$work_dir/$name.normalized"
  sed -n '1,/^$/p' "$work_dir/$name.normalized" >"$OUTPUT_DIR/$name.headers"
  sed '1,/^$/d' "$work_dir/$name.normalized" >"$OUTPUT_DIR/$name.body"
  awk 'NR == 1 { print $2 }' "$work_dir/$name.normalized"
}

status=$(request live /health/live)
[[ "$status" == 204 ]] || fail "liveness returned $status"
status=$(request ready /health/ready)
[[ "$status" == 204 ]] || fail "readiness returned $status"
status=$(request home /)
[[ "$status" == 200 ]] || fail "home SSR returned $status"
grep --fixed-strings --quiet '<title>Manchester Arcana</title>' "$OUTPUT_DIR/home.body" \
  || fail "home SSR omitted title"
grep --fixed-strings --quiet 'Six saved steps to your first adventure' "$OUTPUT_DIR/home.body" \
  || fail "home SSR omitted first-run flow"
grep --ignore-case --extended-regexp --quiet '^content-security-policy:.*frame-ancestors' "$OUTPUT_DIR/home.headers" \
  || fail "home SSR omitted CSP framing protection"
grep --ignore-case --extended-regexp --quiet '^cache-control:[[:space:]]*no-store' "$OUTPUT_DIR/home.headers" \
  || fail "home SSR omitted private no-store cache control"

for page in guide privacy-and-safety legal; do
  status=$(request "$page" "/$page")
  [[ "$status" == 200 ]] || fail "/$page returned $status"
done

status=$(request invalid-host / attacker.invalid)
[[ "$status" == 421 ]] || fail "invalid Host returned $status"
grep --fixed-strings --quiet 'invalid_request_host' "$OUTPUT_DIR/invalid-host.body" \
  || fail "invalid Host response did not use stable public code"

docker stop --time 15 "$container_name" >/dev/null
exit_code=$(docker container inspect "$container_name" --format '{{.State.ExitCode}}')
[[ "$exit_code" == 0 ]] || fail "SIGTERM did not drain cleanly (exit $exit_code)"
docker start "$container_name" >/dev/null
wait_until_ready
status=$(request restarted-ready /health/ready)
[[ "$status" == 204 ]] || fail "readiness after restart returned $status"

docker stop --time 15 "$container_name" >/dev/null
docker logs "$container_name" >"$OUTPUT_DIR/server.log" 2>&1
SECRET_SCAN_CANARIES="$text_canary"$'\n'"$image_canary" \
SECRET_SCAN_REQUIRE_CANARIES=1 \
  scripts/scan-secrets.sh "$OUTPUT_DIR"

echo "container smoke passed: live/ready, SSR/info/legal, boundary headers, graceful stop, restart, and secret canaries"
