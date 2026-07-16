#!/usr/bin/env bash

set -Eeuo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace"
umask 077

source_database_file="${LEGACY_DATABASE_FILE:-data/manchester-arcana.db}"
database_url="${DATABASE_URL:-postgresql://manchester_arcana:manchester_arcana@127.0.0.1:5432/manchester_arcana}"
postgres_service="${LEGACY_POSTGRES_SERVICE:-postgres}"
postgres_user="${LEGACY_POSTGRES_USER:-manchester_arcana}"
recovery_key_file="${RECOVERY_VAULT_KEY_FILE:-.runtime-private/keys/recovery-vault.key}"
backup_root="${LEGACY_BACKUP_ROOT:-.runtime-private/legacy-backups}"
evidence_root="${LEGACY_EVIDENCE_ROOT:-.runtime-private/legacy-import-evidence}"
rng_key_file="${RNG_MASTER_KEY_FILE:-data/rng-master.key}"

for command in cargo docker jq sha256sum tar cmp date stat; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "legacy import drill: required command is unavailable: $command" >&2
        exit 1
    fi
done
if [[ ! -f "$source_database_file" || -L "$source_database_file" ]]; then
    echo "legacy import drill: source must be a real SQLite file" >&2
    exit 1
fi
if [[ -e "$source_database_file-wal" || -e "$source_database_file-shm" ]]; then
    echo "legacy import drill: stop the legacy writer and checkpoint/remove sidecars first" >&2
    exit 1
fi

database_url_without_query="${database_url%%\?*}"
database_query=""
if [[ "$database_url" == *\?* ]]; then
    database_query="?${database_url#*\?}"
fi
database_url_prefix="${database_url_without_query%/*}"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
target_database="manchester_arcana_legacy_import_${timestamp//[^0-9A-Za-z]/_}_$$"
target_database_url="$database_url_prefix/$target_database$database_query"
source_hex_digest="$(sha256sum "$source_database_file" | awk '{print $1}')"
source_digest="sha256:$source_hex_digest"
if [[ ! "$source_digest" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "legacy import drill: source digest is invalid" >&2
    exit 1
fi

drill_root="$(mktemp -d)"
chmod 700 "$drill_root"
backup_bundle="$drill_root/legacy-backup.tar"
opened_bundle="$drill_root/opened.tar"
bundle_root="$drill_root/bundle"
mkdir -m 700 "$bundle_root"
mkdir -p "$backup_root" "$(dirname "$recovery_key_file")" "$evidence_root"
chmod 700 "$backup_root" "$(dirname "$recovery_key_file")" "$evidence_root"
evidence_dir="$evidence_root/$timestamp-$$"
mkdir -m 700 "$evidence_dir"
backup_vault="$backup_root/legacy-manchester-arcana-$timestamp-$$.marv"
target_created=false
drill_passed=false

cleanup() {
    if [[ "$target_created" == true ]]; then
        docker compose exec -T "$postgres_service" \
            dropdb -U "$postgres_user" --if-exists "$target_database" \
            >/dev/null 2>&1 || true
    fi
    rm -rf "$drill_root"
}
trap cleanup EXIT

cargo build --quiet --locked -p manchester-dnd-server \
    --features legacy-import \
    --bin database-migrate --bin legacy-import --bin recovery-vault --bin recovery-manifest
migrate_binary="target/debug/database-migrate"
import_binary="target/debug/legacy-import"
vault_binary="target/debug/recovery-vault"
manifest_binary="target/debug/recovery-manifest"

cp --no-preserve=ownership "$source_database_file" "$bundle_root/manchester-arcana.db"
chmod 600 "$bundle_root/manchester-arcana.db"
sha256sum "$bundle_root/manchester-arcana.db" >"$bundle_root/source.sha256"
tar --format=pax --numeric-owner --owner=0 --group=0 \
    -C "$bundle_root" -cf "$backup_bundle" .
chmod 600 "$backup_bundle"
if [[ ! -f "$recovery_key_file" ]]; then
    "$vault_binary" create-key "$recovery_key_file" >"$evidence_dir/key-created.json"
fi
"$vault_binary" seal "$backup_bundle" "$backup_vault" "$recovery_key_file" \
    >"$evidence_dir/backup-receipt.json"
"$vault_binary" open "$backup_vault" "$opened_bundle" "$recovery_key_file" \
    >"$evidence_dir/backup-open-receipt.json"
cmp "$backup_bundle" "$opened_bundle"

docker compose exec -T "$postgres_service" \
    createdb -U "$postgres_user" "$target_database"
target_created=true
DATABASE_URL="$target_database_url" "$migrate_binary" >"$evidence_dir/migration.json"

wrong_digest="sha256:$(printf '0%.0s' {1..64})"
if DATABASE_URL="$target_database_url" \
    "$import_binary" "$source_database_file" "$wrong_digest" \
    >"$evidence_dir/wrong-digest.stdout" 2>"$evidence_dir/wrong-digest.json"; then
    echo "legacy import drill: wrong source digest was accepted" >&2
    exit 1
fi

DATABASE_URL="$target_database_url" \
    "$import_binary" "$source_database_file" "$source_digest" \
    >"$evidence_dir/first-import.json"
DATABASE_URL="$target_database_url" \
    "$import_binary" "$source_database_file" "$source_digest" \
    >"$evidence_dir/replay-import.json"

jq -e '
    .committed == true
    and .source_counts.campaign_sessions == 1
    and .source_counts.characters == 1
    and .source_counts.turn_audits == 0
    and .inserted_counts.campaign_sessions == 1
    and .inserted_counts.characters == 1
    and .source_state_digest == .target_state_digest
    and .timestamp_match_count == 2
' "$evidence_dir/first-import.json" >/dev/null
jq -e '
    .committed == true
    and .inserted_counts.campaign_sessions == 0
    and .inserted_counts.characters == 0
    and .already_present_counts.campaign_sessions == 1
    and .already_present_counts.characters == 1
    and .source_state_digest == .target_state_digest
' "$evidence_dir/replay-import.json" >/dev/null

DATABASE_URL="$target_database_url" \
RNG_MASTER_KEY_FILE="$rng_key_file" \
IMAGE_ARTIFACT_ROOT="$drill_root/empty-artifacts" \
    "$manifest_binary" >"$evidence_dir/imported-recovery-manifest.json"
jq -e '
    .database.campaigns | length == 1
    and .[0].campaign_session_id == "local-campaign"
    and .[0].campaign_revision == 1
    and .[0].turn_count == 0
' "$evidence_dir/imported-recovery-manifest.json" >/dev/null

source_after_digest="sha256:$(sha256sum "$source_database_file" | awk '{print $1}')"
if [[ "$source_after_digest" != "$source_digest" ]]; then
    echo "legacy import drill: immutable source changed" >&2
    exit 1
fi

backup_vault_digest="$(jq -er '.ok.vault_id' "$evidence_dir/backup-receipt.json")"
jq -n \
    --arg completed_at "$timestamp" \
    --arg source_digest "$source_digest" \
    --arg backup_vault "$(basename "$backup_vault")" \
    --arg backup_vault_digest "$backup_vault_digest" \
    '{
        schema_version: 1,
        result: "passed",
        completed_at_utc: $completed_at,
        source_database_digest: $source_digest,
        encrypted_backup_vault: $backup_vault,
        encrypted_backup_vault_digest: $backup_vault_digest,
        supported_source_migrations: [1, 2],
        exact_replay_idempotent: true,
        wrong_digest_rejected_before_publication: true,
        source_and_target_state_hashes_match: true,
        source_file_unchanged: true,
        isolated_target_dropped_on_exit: true
    }' >"$evidence_dir/drill-summary.json"
chmod 600 "$backup_vault" "$evidence_dir"/*

drill_passed=true
echo "legacy import drill: encrypted source backup, versioned atomic import, exact replay, state hashes, and rollback passed"
echo "legacy import drill: backup=$backup_vault evidence=$evidence_dir"
