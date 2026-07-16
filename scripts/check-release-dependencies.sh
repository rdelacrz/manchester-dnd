#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

CARGO_AUDIT_VERSION=${CARGO_AUDIT_VERSION:-0.22.2}
CARGO_DENY_VERSION=${CARGO_DENY_VERSION:-0.20.2}

for command in cargo-audit cargo-deny npm python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "release dependency check requires: $command" >&2
    exit 1
  fi
done

audit_version=$(cargo-audit audit --version | awk '{print $2}')
deny_version=$(cargo-deny deny --version | awk '{print $2}')
if [[ "$audit_version" != "$CARGO_AUDIT_VERSION" ]]; then
  echo "cargo-audit version mismatch: expected ${CARGO_AUDIT_VERSION}, got ${audit_version}" >&2
  exit 1
fi
if [[ "$deny_version" != "$CARGO_DENY_VERSION" ]]; then
  echo "cargo-deny version mismatch: expected ${CARGO_DENY_VERSION}, got ${deny_version}" >&2
  exit 1
fi

cargo deny --locked check
cargo deny --manifest-path tests/fuzz/Cargo.toml --config tests/fuzz/deny.toml --locked check

# cargo-audit scans every Cargo.lock record, including inactive optional SQLx
# drivers. Keep its sole vulnerability exception conditional on proof that no
# workspace feature/target has a path to the affected rsa crate.
rsa_path=$(cargo tree --locked --all-features --target all -i rsa --prefix none 2>/dev/null)
if [[ -n "$rsa_path" ]]; then
  echo "RUSTSEC-2023-0071 exception is invalid because rsa is reachable:" >&2
  echo "$rsa_path" >&2
  exit 1
fi

cargo audit \
  --deny warnings \
  --ignore RUSTSEC-2023-0071 \
  --ignore RUSTSEC-2024-0436 \
  --ignore RUSTSEC-2026-0173

fuzz_rsa_path=$(cargo tree \
  --manifest-path tests/fuzz/Cargo.toml \
  --locked \
  --all-features \
  --target all \
  -i rsa \
  --prefix none 2>/dev/null)
if [[ -n "$fuzz_rsa_path" ]]; then
  echo "RUSTSEC-2023-0071 fuzz exception is invalid because rsa is reachable:" >&2
  echo "$fuzz_rsa_path" >&2
  exit 1
fi
cargo audit \
  --file tests/fuzz/Cargo.lock \
  --deny warnings \
  --ignore RUSTSEC-2023-0071

python3 scripts/validate_javascript_dependencies.py
npm audit --audit-level=high --omit=optional

echo "release dependency policy passed: Rust advisories/licenses/sources and JavaScript audit/licenses"
