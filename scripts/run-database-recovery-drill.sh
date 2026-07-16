#!/usr/bin/env bash

set -Eeuo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace"
umask 077

database_url="${DATABASE_URL:-postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/manchester_arcana}"
postgres_service="${RECOVERY_POSTGRES_SERVICE:-postgres}"
postgres_user="${RECOVERY_POSTGRES_USER:-manchester_arcana}"
rng_key_file="${RNG_MASTER_KEY_FILE:-data/rng-master.key}"
image_artifact_root="${IMAGE_ARTIFACT_ROOT:-data/generated-images}"
legacy_database_file="${LEGACY_DATABASE_FILE:-data/manchester-arcana.db}"
backup_root="${RECOVERY_BACKUP_ROOT:-.runtime-private/recovery-backups}"
recovery_key_file="${RECOVERY_VAULT_KEY_FILE:-.runtime-private/keys/recovery-vault.key}"
evidence_root="${RECOVERY_EVIDENCE_ROOT:-.runtime-private/recovery-evidence}"
require_image_artifacts="${RECOVERY_REQUIRE_IMAGE_ARTIFACTS:-0}"

for command in cargo docker jq sha256sum tar find cmp date stat df wc; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "database recovery drill: required command is unavailable: $command" >&2
        exit 1
    fi
done
if [[ "$require_image_artifacts" != "0" && "$require_image_artifacts" != "1" ]]; then
    echo "database recovery drill: RECOVERY_REQUIRE_IMAGE_ARTIFACTS must be 0 or 1" >&2
    exit 1
fi

database_url_without_query="${database_url%%\?*}"
database_query=""
if [[ "$database_url" == *\?* ]]; then
    database_query="?${database_url#*\?}"
fi
source_database="${database_url_without_query##*/}"
database_url_prefix="${database_url_without_query%/*}"
if [[ ! "$source_database" =~ ^[A-Za-z0-9_]+$ ]]; then
    echo "database recovery drill: database name must be a simple PostgreSQL identifier" >&2
    exit 1
fi
if [[ ! -f "$rng_key_file" ]]; then
    echo "database recovery drill: RNG master key is missing" >&2
    exit 1
fi
if [[ "$(stat -c '%a' "$rng_key_file")" != "600" ]]; then
    echo "database recovery drill: RNG master key must have mode 0600" >&2
    exit 1
fi

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
drill_root="$(mktemp -d)"
chmod 700 "$drill_root"
bundle_root="$drill_root/bundle"
opened_root="$drill_root/opened"
mkdir -m 700 "$bundle_root" "$opened_root"
mkdir -p "$backup_root" "$(dirname "$recovery_key_file")" "$evidence_root"
chmod 700 "$backup_root" "$(dirname "$recovery_key_file")" "$evidence_root"
evidence_dir="$evidence_root/$timestamp-$$"
mkdir -m 700 "$evidence_dir"
vault_file="$backup_root/manchester-arcana-$timestamp-$$.marv"
bundle_tar="$drill_root/recovery.tar"
opened_tar="$drill_root/opened.tar"
restore_database="manchester_arcana_restore_${timestamp//[^0-9A-Za-z]/_}_$$"
restore_database_url="$database_url_prefix/$restore_database$database_query"
restore_database_created=false
drill_passed=false
vault_digest=""
image_artifact_file_count=0

update_failed_status() {
    if [[ -z "$vault_digest" ]]; then
        return
    fi
    docker compose exec -T "$postgres_service" \
        psql -U "$postgres_user" -d "$source_database" \
        --no-psqlrc --set=ON_ERROR_STOP=1 --set=vault_digest="$vault_digest" \
        --quiet >/dev/null <<'SQL' || true
UPDATE operator_recovery_status
SET last_restore_test_completed_at = CURRENT_TIMESTAMP,
    last_restore_test_result = 'failed',
    last_restore_source_digest = :'vault_digest',
    updated_at = CURRENT_TIMESTAMP
WHERE singleton;
SQL
}

cleanup() {
    if [[ "$restore_database_created" == true ]]; then
        docker compose exec -T "$postgres_service" \
            dropdb -U "$postgres_user" --if-exists "$restore_database" \
            >/dev/null 2>&1 || true
    fi
    if [[ "$drill_passed" != true ]]; then
        update_failed_status
    fi
    rm -rf "$drill_root"
}
trap cleanup EXIT

cargo build --quiet --locked -p manchester-dnd-server \
    --bin database-migrate --bin recovery-manifest --bin recovery-vault --bin database-ops
migrate_binary="target/debug/database-migrate"
manifest_binary="target/debug/recovery-manifest"
vault_binary="target/debug/recovery-vault"
ops_binary="target/debug/database-ops"

DATABASE_URL="$database_url" "$migrate_binary" >"$evidence_dir/migration.json"
DATABASE_URL="$database_url" \
RNG_MASTER_KEY_FILE="$rng_key_file" \
IMAGE_ARTIFACT_ROOT="$image_artifact_root" \
    "$manifest_binary" >"$bundle_root/source-manifest.json"

docker compose exec -T "$postgres_service" \
    pg_dump -U "$postgres_user" -d "$source_database" \
    --format=custom --no-owner --no-acl >"$bundle_root/database.dump"
docker compose exec -T "$postgres_service" \
    pg_restore --list <"$bundle_root/database.dump" >/dev/null

cp --no-preserve=ownership "$rng_key_file" "$bundle_root/rng-master.key"
chmod 600 "$bundle_root/rng-master.key"

if [[ -e "$image_artifact_root" ]]; then
    if [[ ! -d "$image_artifact_root" || -L "$image_artifact_root" ]]; then
        echo "database recovery drill: image artifact root is not a real directory" >&2
        exit 1
    fi
    unsupported_entry="$(find "$image_artifact_root" \( -type l -o \( ! -type d ! -type f \) \) -print -quit)"
    if [[ -n "$unsupported_entry" ]]; then
        echo "database recovery drill: protected artifact tree contains an unsupported entry" >&2
        exit 1
    fi
    mkdir -m 700 "$bundle_root/generated-images"
    cp -a --no-preserve=ownership "$image_artifact_root/." "$bundle_root/generated-images/"
    image_artifact_file_count="$(find "$bundle_root/generated-images" -type f -size +0c | wc -l)"
fi
if [[ "$require_image_artifacts" == "1" && "$image_artifact_file_count" -eq 0 ]]; then
    echo "database recovery drill: a nonempty generated-image artifact was required" >&2
    exit 1
fi

if [[ -e "$legacy_database_file" ]]; then
    if [[ ! -f "$legacy_database_file" || -L "$legacy_database_file" ]]; then
        echo "database recovery drill: legacy database path is not a real file" >&2
        exit 1
    fi
    cp --no-preserve=ownership "$legacy_database_file" "$bundle_root/legacy-manchester-arcana.db"
    chmod 600 "$bundle_root/legacy-manchester-arcana.db"
fi

(
    cd "$bundle_root"
    find . -type f ! -name files.sha256 -print0 \
        | sort -z \
        | xargs -0 sha256sum >files.sha256
    chmod 600 files.sha256
)
tar --format=pax --numeric-owner --owner=0 --group=0 \
    -C "$bundle_root" -cf "$bundle_tar" .
chmod 600 "$bundle_tar"

if [[ ! -f "$recovery_key_file" ]]; then
    "$vault_binary" create-key "$recovery_key_file" >"$evidence_dir/key-created.json"
fi
if [[ "$(stat -c '%a' "$recovery_key_file")" != "600" ]]; then
    echo "database recovery drill: recovery key must have mode 0600" >&2
    exit 1
fi
"$vault_binary" seal "$bundle_tar" "$vault_file" "$recovery_key_file" \
    >"$evidence_dir/vault-receipt.json"
vault_digest="$(jq -er '.ok.vault_id' "$evidence_dir/vault-receipt.json")"
created_at_epoch="$(jq -er '.ok.created_at_epoch' "$evidence_dir/vault-receipt.json")"
if [[ ! "$vault_digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "database recovery drill: recovery vault receipt digest is invalid" >&2
    exit 1
fi
if LC_ALL=C grep -a -F -q 'PGDMP' "$vault_file"; then
    echo "database recovery drill: encrypted vault exposed the PostgreSQL archive header" >&2
    exit 1
fi

"$vault_binary" open "$vault_file" "$opened_tar" "$recovery_key_file" \
    >"$evidence_dir/open-receipt.json"
cmp "$bundle_tar" "$opened_tar"
if tar -tf "$opened_tar" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    echo "database recovery drill: authenticated bundle contains an unsafe path" >&2
    exit 1
fi
tar --extract --file "$opened_tar" --directory "$opened_root" \
    --no-same-owner --no-same-permissions
(
    cd "$opened_root"
    sha256sum --check files.sha256 >/dev/null
)

docker compose exec -T "$postgres_service" \
    createdb -U "$postgres_user" "$restore_database"
restore_database_created=true
docker compose exec -T "$postgres_service" \
    pg_restore -U "$postgres_user" -d "$restore_database" \
    --exit-on-error --no-owner --no-acl <"$opened_root/database.dump"

restored_artifact_root="$opened_root/generated-images"
mkdir -p "$restored_artifact_root"
chmod 700 "$restored_artifact_root"
DATABASE_URL="$restore_database_url" \
RNG_MASTER_KEY_FILE="$opened_root/rng-master.key" \
IMAGE_ARTIFACT_ROOT="$restored_artifact_root" \
    "$manifest_binary" >"$evidence_dir/restored-manifest.json"
cp "$bundle_root/source-manifest.json" "$evidence_dir/source-manifest.json"
cmp "$evidence_dir/source-manifest.json" "$evidence_dir/restored-manifest.json"

expiry_root="$drill_root/expiry"
mkdir -m 700 "$expiry_root"
cp "$vault_file" "$expiry_root/retention-test.marv"
chmod 600 "$expiry_root/retention-test.marv"
"$vault_binary" expire "$expiry_root" "$recovery_key_file" \
    "$((created_at_epoch + 2592000 - 1))" >"$evidence_dir/pre-expiry.json"
test -f "$expiry_root/retention-test.marv"
"$vault_binary" expire "$expiry_root" "$recovery_key_file" \
    "$((created_at_epoch + 2592000))" >"$evidence_dir/at-expiry.json"
test ! -e "$expiry_root/retention-test.marv"

docker compose exec -T "$postgres_service" \
    psql -U "$postgres_user" -d "$source_database" \
    --no-psqlrc --set=ON_ERROR_STOP=1 --set=vault_digest="$vault_digest" \
    --quiet >/dev/null <<'SQL'
UPDATE operator_recovery_status
SET last_backup_completed_at = CURRENT_TIMESTAMP,
    last_backup_vault_digest = :'vault_digest',
    last_restore_test_completed_at = CURRENT_TIMESTAMP,
    last_restore_test_result = 'passed',
    last_restore_source_digest = :'vault_digest',
    updated_at = CURRENT_TIMESTAMP
WHERE singleton;
SQL

DATABASE_URL="$database_url" "$ops_binary" >"$evidence_dir/database-operations.json"
df -Pk "$backup_root" >"$evidence_dir/disk-capacity.txt"
jq -n \
    --arg completed_at "$timestamp" \
    --arg vault_file "$(basename "$vault_file")" \
    --arg vault_digest "$vault_digest" \
    --arg source_database "$source_database" \
    --argjson image_artifact_file_count "$image_artifact_file_count" \
    '{
        schema_version: 1,
        result: "passed",
        completed_at_utc: $completed_at,
        source_database: $source_database,
        isolated_restore_database_was_dropped: true,
        vault_file: $vault_file,
        vault_digest: $vault_digest,
        exact_manifest_match: true,
        nonempty_image_artifact_files_restored: $image_artifact_file_count,
        exact_30_day_expiry_boundary_verified: true
    }' >"$evidence_dir/drill-summary.json"
chmod 600 "$evidence_dir"/* "$vault_file"

drill_passed=true
echo "database recovery drill: encrypted backup, isolated restore, canonical campaign/artifact/key match, and exact 30-day expiry passed"
echo "database recovery drill: vault=$vault_file evidence=$evidence_dir"
