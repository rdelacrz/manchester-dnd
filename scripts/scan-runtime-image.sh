#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

IMAGE=${1:?usage: scan-runtime-image.sh IMAGE [OUTPUT_DIR]}
OUTPUT_DIR=${2:-target/release-evidence/container-scan}
MAX_IMAGE_BYTES=${MAX_IMAGE_BYTES:-134217728}
SYFT_VERSION=${SYFT_VERSION:-1.46.0}
GRYPE_VERSION=${GRYPE_VERSION:-0.115.0}

for command in docker syft grype jq nm python3; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "runtime image scan requires: $command" >&2
    exit 1
  fi
done

[[ "$(syft version -o json | jq -r .version)" == "$SYFT_VERSION" ]]
[[ "$(grype version -o json | jq -r .version)" == "$GRYPE_VERSION" ]]

install -d -m 0755 "$OUTPUT_DIR"
work_dir=$(mktemp -d)
container_id=
cleanup() {
  if [[ -n "$container_id" ]]; then
    docker container rm "$container_id" >/dev/null 2>&1 || true
  fi
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

docker image inspect "$IMAGE" >"$OUTPUT_DIR/image-inspect.json"
image_size=$(jq -r '.[0].Size' "$OUTPUT_DIR/image-inspect.json")
image_user=$(jq -r '.[0].Config.User' "$OUTPUT_DIR/image-inspect.json")
if (( image_size > MAX_IMAGE_BYTES )); then
  echo "runtime image exceeds ${MAX_IMAGE_BYTES} bytes: ${image_size}" >&2
  exit 1
fi
if [[ "$image_user" != "65532:65532" ]]; then
  echo "runtime image must use explicit nonroot 65532:65532, got ${image_user}" >&2
  exit 1
fi

docker image save --output "$work_dir/image.tar" "$IMAGE"
container_id=$(docker container create "$IMAGE")
docker container export --output "$work_dir/rootfs.tar" "$container_id"
docker container rm "$container_id" >/dev/null
container_id=

python3 scripts/validate_runtime_rootfs.py "$work_dir/rootfs.tar"
scripts/scan-secrets.sh "$work_dir/rootfs.tar"
tar --extract --to-stdout --file "$work_dir/rootfs.tar" \
  usr/local/bin/manchester-dnd-web >"$work_dir/manchester-dnd-web"
if nm --dynamic --undefined-only "$work_dir/manchester-dnd-web" \
  | grep --extended-regexp --quiet \
    '(^|[[:space:]])(__isoc99_)?(scanf|fscanf|sscanf|vscanf|vfscanf|vsscanf|ungetwc|ns_printrrf|ns_printrr|fp_nquery)(@|$)'; then
  echo "runtime libc exception is invalid: affected symbol is imported" >&2
  exit 1
fi

syft "docker-archive:$work_dir/image.tar" \
  --quiet \
  --output "cyclonedx-json=$OUTPUT_DIR/runtime.cdx.json"
jq -e '
  .bomFormat == "CycloneDX" and
  (.specVersion | type == "string") and
  (.components | type == "array" and length > 0)
' "$OUTPUT_DIR/runtime.cdx.json" >/dev/null

if ! grype "sbom:$OUTPUT_DIR/runtime.cdx.json" \
  --config .grype.yaml \
  --fail-on high \
  --output json >"$OUTPUT_DIR/vulnerabilities.json"; then
  jq -r '
    [.matches[] | select(.vulnerability.severity == "High" or .vulnerability.severity == "Critical")]
    | group_by(.vulnerability.id)
    | .[]
    | "\(.[0].vulnerability.severity) \(.[0].vulnerability.id) \(.[0].artifact.name)@\(.[0].artifact.version)"
  ' "$OUTPUT_DIR/vulnerabilities.json" >&2
  echo "runtime image vulnerability gate failed" >&2
  exit 1
fi

jq -e '
  ([.ignoredMatches[].vulnerability.id] | sort) ==
    ["CVE-2026-5435", "CVE-2026-5450", "CVE-2026-5928"] and
  ([.ignoredMatches[] | .artifact.name] | unique) == ["libc6"] and
  ([.ignoredMatches[] | .artifact.version] | unique) == ["2.41-12+deb13u3"] and
  ([.ignoredMatches[] | .artifact.type] | unique) == ["deb"] and
  ([.matches[] | select(
    .vulnerability.severity == "High" or .vulnerability.severity == "Critical"
  )] | length) == 0
' "$OUTPUT_DIR/vulnerabilities.json" >/dev/null || {
  echo "runtime vulnerability exception set is stale, broadened, or incomplete" >&2
  exit 1
}

python3 scripts/record_release_provenance.py \
  --image "$IMAGE" \
  --sbom "$OUTPUT_DIR/runtime.cdx.json" \
  --output "$OUTPUT_DIR/provenance.intoto.json"

(
  cd "$OUTPUT_DIR"
  sha256sum -- image-inspect.json provenance.intoto.json runtime.cdx.json vulnerabilities.json \
    | LC_ALL=C sort >SHA256SUMS
)

echo "runtime image scan passed: $IMAGE"
echo "  Size: ${image_size} bytes (limit ${MAX_IMAGE_BYTES})"
echo "  Components: $(jq '.components | length' "$OUTPUT_DIR/runtime.cdx.json")"
echo "  High/critical vulnerabilities: 0"
echo "  Accepted non-reachable libc findings: 3 (exact, symbol-gated, review by 2026-08-15)"
