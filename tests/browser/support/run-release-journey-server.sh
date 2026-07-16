#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

host="${JOURNEY_PGHOST:-127.0.0.1}"
port="${JOURNEY_PGPORT:-5432}"
user="${JOURNEY_PGUSER:-manchester_arcana}"
database="${JOURNEY_PGDATABASE:-manchester_arcana_release_journey}"
export PGPASSWORD="${JOURNEY_PGPASSWORD:-manchester_arcana}"

restart_request="${JOURNEY_RESTART_REQUEST:-target/playwright/release-journey-restart.request}"
restart_ack="${JOURNEY_RESTART_ACK:-target/playwright/release-journey-restart.ack}"
server_log="${JOURNEY_SERVER_LOG:-target/playwright/release-journey-server.log}"
image_root="${IMAGE_ARTIFACT_ROOT:-.runtime-private/playwright/release-journey/images}"
rng_key="${RNG_MASTER_KEY_FILE:-.runtime-private/playwright/release-journey/rng-master.key}"

mkdir -p "$(dirname "$restart_request")" "$(dirname "$server_log")" "$(dirname "$rng_key")"
chmod 0700 "$(dirname "$rng_key")"
rm -f "$restart_request" "$restart_ack" "$rng_key"
rm -rf "$image_root"
umask 077
for _ in {1..32}; do
  printf '\007'
done >"$rng_key"
chmod 0600 "$rng_key"
: >"$server_log"
exec > >(tee "$server_log") 2>&1

# The database and private runtime artifacts are disposable for this release
# journey. A restart below preserves them; only initial supervisor startup
# clears them.
if command -v dropdb >/dev/null 2>&1 && command -v createdb >/dev/null 2>&1; then
  dropdb --if-exists --force --host "$host" --port "$port" --username "$user" "$database"
  createdb --host "$host" --port "$port" --username "$user" "$database"
elif command -v docker >/dev/null 2>&1; then
  docker compose exec -T postgres dropdb --if-exists --force --username "$user" "$database"
  docker compose exec -T postgres createdb --username "$user" "$database"
else
  echo "release journey requires PostgreSQL client tools or Docker Compose" >&2
  exit 1
fi

export DATABASE_URL="postgresql://${user}:${PGPASSWORD}@${host}:${port}/${database}"
export IMAGE_ARTIFACT_ROOT="$image_root"
export RNG_MASTER_KEY_FILE="$rng_key"

child_pid=""
restart_count=0

start_server() {
  target/release/manchester-dnd-web &
  child_pid=$!
}

stop_server() {
  if [[ -n "$child_pid" ]] && kill -0 "$child_pid" 2>/dev/null; then
    kill -TERM "$child_pid"
    wait "$child_pid" || true
  fi
  child_pid=""
}

shutdown() {
  trap - EXIT INT TERM
  stop_server
  exit 0
}
trap shutdown EXIT INT TERM

start_server
while true; do
  if ! kill -0 "$child_pid" 2>/dev/null; then
    wait "$child_pid"
    echo "release-journey child exited before a requested restart" >&2
    exit 1
  fi

  if [[ -f "$restart_request" ]]; then
    rm -f "$restart_request"
    stop_server
    restart_count=$((restart_count + 1))
    start_server
    tmp_ack="${restart_ack}.tmp"
    printf '%s\n' "$restart_count" >"$tmp_ack"
    mv "$tmp_ack" "$restart_ack"
  fi
  sleep 0.1
done
