#!/usr/bin/env bash

set -Eeuo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace"

postgres_service="${POSTGRES_ROLE_SERVICE:-postgres}"
postgres_user="${POSTGRES_ROLE_ADMIN_USER:-manchester_arcana}"
database_name="${POSTGRES_ROLE_DATABASE:-manchester_arcana}"

docker compose exec -T "$postgres_service" \
    psql -U "$postgres_user" -d "$database_name" \
    --no-psqlrc --set=ON_ERROR_STOP=1 \
    --file=- <scripts/postgres-roles.sql >/dev/null

result="$(docker compose exec -T "$postgres_service" \
    psql -U "$postgres_user" -d "$database_name" \
    --no-psqlrc --set=ON_ERROR_STOP=1 --tuples-only --no-align <<'SQL'
WITH role_shape AS (
    SELECT bool_and(
        NOT rolcanlogin
        AND NOT rolsuper
        AND NOT rolcreatedb
        AND NOT rolcreaterole
        AND NOT rolreplication
        AND NOT rolbypassrls
    ) AS safe
    FROM pg_roles
    WHERE rolname IN (
        'manchester_arcana_migration',
        'manchester_arcana_app',
        'manchester_arcana_backup',
        'manchester_arcana_operator'
    )
), privilege_shape AS (
    SELECT
        has_table_privilege('manchester_arcana_app', 'campaign_sessions', 'SELECT')
        AND has_table_privilege('manchester_arcana_app', 'campaign_sessions', 'INSERT')
        AND NOT has_table_privilege(
            'manchester_arcana_app', 'operator_recovery_status', 'UPDATE'
        )
        AND has_table_privilege(
            'manchester_arcana_backup', 'campaign_sessions', 'SELECT'
        )
        AND NOT has_table_privilege(
            'manchester_arcana_backup', 'campaign_sessions', 'INSERT'
        )
        AND has_table_privilege(
            'manchester_arcana_operator', 'operator_recovery_status', 'SELECT'
        )
        AND NOT has_table_privilege(
            'manchester_arcana_operator', 'campaign_sessions', 'SELECT'
        ) AS safe
)
SELECT role_shape.safe AND privilege_shape.safe
FROM role_shape CROSS JOIN privilege_shape;
SQL
)"

if [[ "$result" != "t" ]]; then
    echo "PostgreSQL role policy: least-privilege assertions failed" >&2
    exit 1
fi

echo "PostgreSQL role policy: migration/app/backup/operator attributes and grants passed"
