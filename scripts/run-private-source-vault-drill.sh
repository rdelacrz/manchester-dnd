#!/usr/bin/env bash
set -euo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
drill_root="$(mktemp -d)"
trap 'rm -rf "$drill_root"' EXIT

source_root="$drill_root/source"
backup_root="$drill_root/backups"
restore_root="$drill_root/restored"
key_file="$drill_root/operator-only.key"
vault_file="$backup_root/source-backup.mavlt"
mkdir -m 700 "$source_root" "$backup_root"

canary="PRIVATE_SOURCE_VAULT_CANARY_7d34f0d447d948d5"
printf '%s\n' \
  '---' \
  '{"schema_version":1}' \
  '---' \
  '' \
  "$canary" >"$source_root/private.md"
chmod 600 "$source_root/private.md"

cargo run --quiet --locked -p manchester-dnd-server --bin source-vault -- \
  create-key "$key_file" >/dev/null
cargo run --quiet --locked -p manchester-dnd-server --bin source-vault -- \
  seal "$source_root" "$vault_file" "$key_file" >/dev/null

if LC_ALL=C grep -a -F -q "$canary" "$vault_file"; then
  echo "source vault drill: encrypted backup exposed the raw canary" >&2
  exit 1
fi

cargo run --quiet --locked -p manchester-dnd-server --bin source-vault -- \
  restore "$vault_file" "$restore_root" "$key_file" >/dev/null
cmp "$source_root/private.md" "$restore_root/private.md"

created_at_epoch="$(
  cargo run --quiet --locked -p manchester-dnd-server --bin source-vault -- \
    inspect "$vault_file" "$key_file" \
    | sed -n 's/.*"created_at_epoch": \([0-9][0-9]*\).*/\1/p'
)"
test -n "$created_at_epoch"
retention_seconds=2592000
cargo run --quiet --locked -p manchester-dnd-server --bin source-vault -- \
  expire "$backup_root" "$key_file" "$((created_at_epoch + retention_seconds - 1))" >/dev/null
test -f "$vault_file"

cargo run --quiet --locked -p manchester-dnd-server --bin source-vault -- \
  expire "$backup_root" "$key_file" "$((created_at_epoch + retention_seconds))" >/dev/null
test ! -e "$vault_file"

echo "source vault drill: authenticated encryption, exact restore, pre-cutoff retention, and at-cutoff expiry passed"
