#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

canaries=(
  "SYNTHETIC_TITLE_CANARY_7F2A91"
  "SYNTHETIC_RAW_SOURCE_CANARY_4D8C63"
  "participant:11111111111111111111111111111111"
  "event-source-dcd82673ccf92b8ce9ba3c4f"
)

scan_paths=(
  target/site
  target/release/manchester-dnd-web
  docs/evidence/typed-gm-v1.json
  docs/evidence/typed-gm-v2.json
)

for path in "${scan_paths[@]}"; do
  if [[ ! -e "$path" ]]; then
    echo "private-inspiration boundary scan: required artifact is missing: $path" >&2
    exit 1
  fi
done

for canary in "${canaries[@]}"; do
  if rg --text --fixed-strings --quiet -- "$canary" "${scan_paths[@]}"; then
    echo "private-inspiration boundary scan: protected canary reached a release/evaluation artifact" >&2
    exit 1
  fi
done

if [[ -n "${PRIVATE_INSPIRATION_LOG_PATH:-}" ]]; then
  if [[ ! -f "$PRIVATE_INSPIRATION_LOG_PATH" ]]; then
    echo "private-inspiration boundary scan: configured log evidence is missing" >&2
    exit 1
  fi
  for canary in "${canaries[@]}"; do
    if rg --text --fixed-strings --quiet -- "$canary" "$PRIVATE_INSPIRATION_LOG_PATH"; then
      echo "private-inspiration boundary scan: protected canary reached normal logs" >&2
      exit 1
    fi
  done
fi

echo "private-inspiration boundary scan: release, source-map, evaluation, and configured log artifacts are clean"
