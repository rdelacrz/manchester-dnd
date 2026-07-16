#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

host="${SLICE6_PGHOST:-127.0.0.1}"
port="${SLICE6_PGPORT:-5432}"
user="${SLICE6_PGUSER:-manchester_arcana}"
database="${SLICE6_PGDATABASE:-manchester_arcana_slice6_browser}"
export PGPASSWORD="${SLICE6_PGPASSWORD:-manchester_arcana}"

if command -v dropdb >/dev/null 2>&1 && command -v createdb >/dev/null 2>&1; then
  dropdb --if-exists --force --host "$host" --port "$port" --username "$user" "$database"
  createdb --host "$host" --port "$port" --username "$user" "$database"
elif command -v docker >/dev/null 2>&1; then
  docker compose exec -T postgres dropdb --if-exists --force --username "$user" "$database"
  docker compose exec -T postgres createdb --username "$user" "$database"
else
  echo "slice 6 browser evidence requires PostgreSQL client tools or Docker Compose" >&2
  exit 1
fi

rm -rf .runtime-private/playwright/slice6/images
export DATABASE_URL="postgresql://${user}:${PGPASSWORD}@${host}:${port}/${database}"
exec target/release/manchester-dnd-web
