#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"
umask 077

source_database="${MONGODB_DATABASE:-manchester_dnd}"
mongodb_uri="${MONGODB_TEST_URI:-${MONGODB_URI:-mongodb://root:dev-root-password@127.0.0.1:27017/?authSource=admin&replicaSet=rs0&directConnection=true}}"
rng_key_file="${RNG_MASTER_KEY_FILE:-data/rng-master.key}"
image_artifact_root="${IMAGE_ARTIFACT_ROOT:-data/generated-images}"
recovery_key_file="${RECOVERY_VAULT_KEY_FILE:-.runtime-private/keys/recovery-vault.key}"
backup_root="${RECOVERY_BACKUP_ROOT:-.runtime-private/recovery-backups}"
evidence_root="${RECOVERY_EVIDENCE_ROOT:-.runtime-private/recovery-evidence}"
mongodb_service="${RECOVERY_MONGODB_SERVICE:-mongodb}"

if [[ ! "$source_database" =~ ^[A-Za-z0-9_-]+$ ]]; then
  echo "database recovery drill: MongoDB database name must be allowlisted" >&2
  exit 1
fi
if [[ "$mongodb_uri" != mongodb://root:*@127.0.0.1:* ]]; then
  echo "database recovery drill: use the explicit loopback root MongoDB URI" >&2
  exit 1
fi
for command in docker jq tar cmp sha256sum; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "database recovery drill: required command is unavailable: $command" >&2
    exit 1
  }
done

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
restore_database="mdnd_restore_${timestamp//[^0-9A-Za-z]/_}_$$"
drill_root="$(mktemp -d)"
bundle_root="$drill_root/bundle"
opened_root="$drill_root/opened"
bundle_tar="$drill_root/mongodb-recovery.tar"
opened_tar="$drill_root/opened.tar"
vault_file="$backup_root/mongodb-${timestamp}.marv"
evidence_dir="$evidence_root/$timestamp"
mkdir -p "$bundle_root" "$opened_root" "$backup_root" "$evidence_dir" "$(dirname "$recovery_key_file")"
chmod 0700 "$backup_root" "$evidence_root" "$evidence_dir"

cleanup() {
  docker compose exec -T "$mongodb_service" bash -ec '
    mongosh --quiet --host 127.0.0.1 \
      --username "$MONGO_INITDB_ROOT_USERNAME" \
      --password "$MONGO_INITDB_ROOT_PASSWORD" \
      --authenticationDatabase admin \
      --eval "db.getSiblingDB(\"'"$restore_database"'\").dropDatabase()" >/dev/null
  ' >/dev/null 2>&1 || true
  rm -rf "$drill_root"
}
trap cleanup EXIT

cargo build --quiet --locked -p manchester-dnd-server \
  --bin mongo-admin --bin database-ops --bin recovery-manifest --bin recovery-vault

vault_binary="target/debug/recovery-vault"
manifest_binary="target/debug/recovery-manifest"
ops_binary="target/debug/database-ops"
admin_binary="target/debug/mongo-admin"

if [[ ! -f "$recovery_key_file" ]]; then
  "$vault_binary" create-key "$recovery_key_file" >"$evidence_dir/key-created.json"
fi
chmod 0600 "$recovery_key_file"

MONGODB_URI="$mongodb_uri" MONGODB_SCHEMA_URI="$mongodb_uri" MONGODB_DATABASE="$source_database" \
  "$admin_binary" schema apply >"$evidence_dir/schema-apply.txt"
MONGODB_URI="$mongodb_uri" MONGODB_DATABASE="$source_database" \
RNG_MASTER_KEY_FILE="$rng_key_file" IMAGE_ARTIFACT_ROOT="$image_artifact_root" \
  "$manifest_binary" >"$bundle_root/source-manifest.json"
MONGODB_URI="$mongodb_uri" MONGODB_DATABASE="$source_database" \
  "$ops_binary" >"$evidence_dir/source-operations.json"

# DragonflyDB is deliberately absent: every recoverable record comes from MongoDB.
docker compose exec -T "$mongodb_service" bash -ec '
  exec mongodump --quiet --host 127.0.0.1 \
    --username "$MONGO_INITDB_ROOT_USERNAME" \
    --password "$MONGO_INITDB_ROOT_PASSWORD" \
    --authenticationDatabase admin \
    --db "$1" --archive --gzip
' -- "$source_database" >"$bundle_root/mongodb.archive.gz"

if [[ -f "$rng_key_file" ]]; then
  install -m 0600 "$rng_key_file" "$bundle_root/rng-master.key"
fi
if [[ -d "$image_artifact_root" ]]; then
  tar -C "$image_artifact_root" -cf "$bundle_root/generated-images.tar" .
fi
printf '%s\n' 'DragonflyDB is disposable and intentionally excluded from recovery bundles.' \
  >"$bundle_root/DRAGONFLY-EXCLUDED.txt"

tar -C "$bundle_root" -cf "$bundle_tar" .
"$vault_binary" seal "$bundle_tar" "$vault_file" "$recovery_key_file" \
  >"$evidence_dir/vault-seal.json"
"$vault_binary" inspect "$vault_file" "$recovery_key_file" \
  >"$evidence_dir/vault-inspect.json"
"$vault_binary" open "$vault_file" "$opened_tar" "$recovery_key_file" \
  >"$evidence_dir/vault-open.json"
cmp "$bundle_tar" "$opened_tar"
tar -C "$opened_root" -xf "$opened_tar"

# Restore to a fresh namespace and verify validators/indexes plus collection counts.
docker compose exec -T "$mongodb_service" bash -ec '
  exec mongorestore --quiet --host 127.0.0.1 \
    --username "$MONGO_INITDB_ROOT_USERNAME" \
    --password "$MONGO_INITDB_ROOT_PASSWORD" \
    --authenticationDatabase admin \
    --archive --gzip --drop \
    --nsFrom "$1.*" --nsTo "$2.*"
' -- "$source_database" "$restore_database" <"$opened_root/mongodb.archive.gz"

MONGODB_URI="$mongodb_uri" MONGODB_DATABASE="$restore_database" \
  "$admin_binary" schema verify >"$evidence_dir/restored-schema-verify.txt"

docker compose exec -T "$mongodb_service" bash -ec '
  mongosh --quiet --host 127.0.0.1 \
    --username "$MONGO_INITDB_ROOT_USERNAME" \
    --password "$MONGO_INITDB_ROOT_PASSWORD" \
    --authenticationDatabase admin \
    --eval "
      const source=db.getSiblingDB(\"$1\");
      const restored=db.getSiblingDB(\"$2\");
      const names=source.getCollectionNames().filter(n => !n.startsWith(\"system.\")).sort();
      const result=names.map(name => ({name, source:source[name].countDocuments({}), restored:restored[name].countDocuments({})}));
      if (result.some(row => row.source !== row.restored)) { print(JSON.stringify(result)); quit(1); }
      print(JSON.stringify(result));
    "
' -- "$source_database" "$restore_database" >"$evidence_dir/document-counts.json"

sha256sum "$vault_file" >"$evidence_dir/vault.sha256"
jq -n \
  --arg source_database "$source_database" \
  --arg restore_database "$restore_database" \
  --arg vault "$vault_file" \
  '{schema:"manchester-arcana/mongodb-recovery-drill/v1", source_database:$source_database, restore_database:$restore_database, encrypted_vault:$vault, dragonfly_excluded:true, schema_verified:true, document_counts_match:true}' \
  >"$evidence_dir/SUMMARY.json"

echo "MongoDB recovery drill passed; evidence: $evidence_dir"
