#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

host="${SLICE3_PGHOST:-127.0.0.1}"
port="${SLICE3_PGPORT:-5432}"
user="${SLICE3_PGUSER:-manchester_arcana}"
database="${SLICE3_PGDATABASE:-manchester_arcana_slice3_browser}"
export PGPASSWORD="${SLICE3_PGPASSWORD:-manchester_arcana}"

# This dedicated database keeps the fixed local campaign IDs isolated from the
# longer provider-disabled browser journey. No user/development database is
# dropped by this evidence runner.
if command -v dropdb >/dev/null 2>&1 && command -v createdb >/dev/null 2>&1; then
  dropdb --if-exists --force --host "$host" --port "$port" --username "$user" "$database"
  createdb --host "$host" --port "$port" --username "$user" "$database"
elif command -v docker >/dev/null 2>&1; then
  docker compose exec -T postgres dropdb --if-exists --force --username "$user" "$database"
  docker compose exec -T postgres createdb --username "$user" "$database"
else
  echo "slice 3 browser evidence requires PostgreSQL client tools or Docker Compose" >&2
  exit 1
fi

export DATABASE_URL="postgresql://${user}:${PGPASSWORD}@${host}:${port}/${database}"
exec target/release/manchester-dnd-web
