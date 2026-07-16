#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

host="${SLICE5_PGHOST:-127.0.0.1}"
port="${SLICE5_PGPORT:-5432}"
user="${SLICE5_PGUSER:-manchester_arcana}"
database="${SLICE5_PGDATABASE:-manchester_arcana_slice5_browser}"
export PGPASSWORD="${SLICE5_PGPASSWORD:-manchester_arcana}"

# This disposable database isolates the consent/privacy journey from local data.
if command -v dropdb >/dev/null 2>&1 && command -v createdb >/dev/null 2>&1; then
  dropdb --if-exists --force --host "$host" --port "$port" --username "$user" "$database"
  createdb --host "$host" --port "$port" --username "$user" "$database"
elif command -v docker >/dev/null 2>&1; then
  docker compose exec -T postgres dropdb --if-exists --force --username "$user" "$database"
  docker compose exec -T postgres createdb --username "$user" "$database"
else
  echo "slice 5 browser evidence requires PostgreSQL client tools or Docker Compose" >&2
  exit 1
fi

export DATABASE_URL="postgresql://${user}:${PGPASSWORD}@${host}:${port}/${database}"
log_path="${PRIVATE_INSPIRATION_LOG_PATH:-target/playwright/slice5-server.log}"
mkdir -p "$(dirname "$log_path")"
: >"$log_path"
exec > >(tee "$log_path") 2>&1
exec target/release/manchester-dnd-web
