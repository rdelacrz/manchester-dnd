#!/usr/bin/env bash

set -Eeuo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
migration_dir="${MIGRATION_DIR:-$root_dir/migrations}"
static_only=false

if [[ "${1:-}" == "--static-only" ]]; then
    static_only=true
elif [[ $# -ne 0 ]]; then
    echo "usage: $0 [--static-only]" >&2
    exit 2
fi

if [[ ! -d "$migration_dir" ]]; then
    echo "migration validation: directory not found: $migration_dir" >&2
    exit 1
fi

mapfile -t migrations < <(find "$migration_dir" -maxdepth 1 -type f -name '*.sql' -print | LC_ALL=C sort)
if [[ ${#migrations[@]} -eq 0 ]]; then
    echo "migration validation: no SQL migrations found" >&2
    exit 1
fi

expected_version=1
declare -A seen_versions=()
for migration in "${migrations[@]}"; do
    filename="$(basename "$migration")"
    if [[ ! "$filename" =~ ^([0-9]{4})_([a-z0-9]+(_[a-z0-9]+)*)\.sql$ ]]; then
        echo "migration validation: invalid filename: $filename" >&2
        echo "expected NNNN_lower_snake_case.sql" >&2
        exit 1
    fi

    version="${BASH_REMATCH[1]}"
    numeric_version=$((10#$version))
    if [[ -n "${seen_versions[$version]:-}" ]]; then
        echo "migration validation: duplicate version: $version" >&2
        exit 1
    fi
    seen_versions[$version]=1

    if (( numeric_version != expected_version )); then
        printf 'migration validation: expected version %04d, found %s\n' "$expected_version" "$version" >&2
        exit 1
    fi
    ((expected_version += 1))

    if [[ ! -s "$migration" ]]; then
        echo "migration validation: empty migration: $filename" >&2
        exit 1
    fi
    if LC_ALL=C grep -q $'\r' "$migration"; then
        echo "migration validation: CRLF line endings are not allowed: $filename" >&2
        exit 1
    fi
done

echo "migration validation: ${#migrations[@]} ordered migration files passed static checks"

if [[ "$static_only" == true ]]; then
    exit 0
fi

database_url="${MIGRATION_DATABASE_URL:-${DATABASE_URL:-}}"
if [[ -z "$database_url" ]]; then
    echo "migration validation: DATABASE_URL or MIGRATION_DATABASE_URL is required" >&2
    exit 1
fi
if ! command -v psql >/dev/null 2>&1; then
    echo "migration validation: psql is required for database validation" >&2
    exit 1
fi

# An isolated schema makes the check safe for a developer database while still
# exercising PostgreSQL syntax, ordering, constraints, and indexes.
schema_name="migration_check_${BASHPID}_${RANDOM}"
cleanup() {
    psql "$database_url" --no-psqlrc --set ON_ERROR_STOP=1 \
        --command "DROP SCHEMA IF EXISTS \"$schema_name\" CASCADE" >/dev/null 2>&1 || true
}
trap cleanup EXIT

psql "$database_url" --no-psqlrc --set ON_ERROR_STOP=1 \
    --command "CREATE SCHEMA \"$schema_name\"" >/dev/null

for migration in "${migrations[@]}"; do
    PGOPTIONS="-c search_path=$schema_name,public" \
        psql "$database_url" --no-psqlrc --set ON_ERROR_STOP=1 \
        --single-transaction --file "$migration" >/dev/null
done

table_count="$(
    psql "$database_url" --no-psqlrc --tuples-only --no-align \
        --set ON_ERROR_STOP=1 \
        --command "SELECT count(*) FROM pg_tables WHERE schemaname = '$schema_name'"
)"
if [[ ! "$table_count" =~ ^[0-9]+$ ]] || (( table_count == 0 )); then
    echo "migration validation: migrations created no tables" >&2
    exit 1
fi

for required_table in campaign_sessions characters turn_audits generated_assets command_receipts; do
    relation="$(
        psql "$database_url" --no-psqlrc --tuples-only --no-align \
            --set ON_ERROR_STOP=1 \
            --command "SELECT to_regclass('$schema_name.$required_table') IS NOT NULL"
    )"
    if [[ "$relation" != "t" ]]; then
        echo "migration validation: required table was not created: $required_table" >&2
        exit 1
    fi
done

echo "migration validation: PostgreSQL applied every migration in an isolated schema ($table_count tables)"
