#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

OUTPUT_DIR=${1:-target/release-evidence/sbom}
CARGO_CYCLONEDX_VERSION=${CARGO_CYCLONEDX_VERSION:-0.5.9}
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
export SOURCE_DATE_EPOCH

if ! command -v cargo-cyclonedx >/dev/null 2>&1; then
  echo "cargo-cyclonedx ${CARGO_CYCLONEDX_VERSION} is required" >&2
  exit 1
fi

actual_version=$(cargo-cyclonedx cyclonedx --version | awk '{print $2}')
if [[ "$actual_version" != "$CARGO_CYCLONEDX_VERSION" ]]; then
  echo "cargo-cyclonedx version mismatch: expected ${CARGO_CYCLONEDX_VERSION}, got ${actual_version}" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required to validate generated SBOMs" >&2
  exit 1
fi

install -d -m 0755 "$OUTPUT_DIR"
prefix=manchester-arcana-workspace

cargo cyclonedx \
  --format json \
  --all-features \
  --target all \
  --spec-version 1.5 \
  --override-filename "$prefix"

declare -A rust_boms=(
  ["app/${prefix}.json"]="rust-app.cdx.json"
  ["crates/game-core/${prefix}.json"]="rust-game-core.cdx.json"
  ["crates/game-server/${prefix}.json"]="rust-game-server.cdx.json"
  ["frontend/${prefix}.json"]="rust-frontend.cdx.json"
  ["server/${prefix}.json"]="rust-web.cdx.json"
)

for source in "${!rust_boms[@]}"; do
  destination="$OUTPUT_DIR/${rust_boms[$source]}"
  if [[ ! -f "$source" ]]; then
    echo "cargo-cyclonedx did not emit expected workspace SBOM: $source" >&2
    exit 1
  fi
  mv -- "$source" "$destination"
done

cargo cyclonedx \
  --manifest-path tests/fuzz/Cargo.toml \
  --format json \
  --all-features \
  --target all \
  --spec-version 1.5 \
  --override-filename manchester-arcana-fuzz
if [[ ! -f tests/fuzz/manchester-arcana-fuzz.json ]]; then
  echo "cargo-cyclonedx did not emit expected fuzz-harness SBOM" >&2
  exit 1
fi
mv -- tests/fuzz/manchester-arcana-fuzz.json "$OUTPUT_DIR/rust-fuzz-harness.cdx.json"

npm_bom="$OUTPUT_DIR/javascript-build.cdx.json"
npm_bom_raw="$OUTPUT_DIR/.javascript-build.cdx.raw.json"
npm sbom --sbom-format cyclonedx --omit=optional >"$npm_bom_raw"
lock_digest=$(sha256sum package-lock.json | awk '{print $1}')
npm_serial="urn:uuid:${lock_digest:0:8}-${lock_digest:8:4}-5${lock_digest:13:3}-a${lock_digest:17:3}-${lock_digest:20:12}"
jq --arg serial "$npm_serial" '
  .serialNumber = $serial |
  .metadata.timestamp = "1970-01-01T00:00:00.000Z"
' "$npm_bom_raw" >"$npm_bom"
rm -- "$npm_bom_raw"
cp -- content/provenance-manifest.json "$OUTPUT_DIR/release-assets.provenance.json"

for bom in "$OUTPUT_DIR"/*.cdx.json; do
  jq -e '
    .bomFormat == "CycloneDX" and
    .specVersion == "1.5" and
    (.metadata.component.name | type == "string") and
    (.components | type == "array")
  ' "$bom" >/dev/null
done

python3 scripts/validate_release_provenance.py >/dev/null

(
  cd "$OUTPUT_DIR"
  sha256sum -- *.json | LC_ALL=C sort >SHA256SUMS
)

echo "release SBOM generated: $OUTPUT_DIR"
echo "  Rust workspace and fuzz-harness components: 6 CycloneDX documents"
echo "  JavaScript build/test components: $(jq '.components | length' "$OUTPUT_DIR/javascript-build.cdx.json")"
echo "  Release assets: $(jq '.assets | length' "$OUTPUT_DIR/release-assets.provenance.json")"
