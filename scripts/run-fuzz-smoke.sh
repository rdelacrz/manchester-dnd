#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

FUZZ_TOOLCHAIN=${FUZZ_TOOLCHAIN:-nightly-2026-06-12}
FUZZ_RUNS=${FUZZ_RUNS:-1000}
OUTPUT_DIR=${FUZZ_OUTPUT_DIR:-target/release-evidence/fuzz-smoke}
EXPECTED_RUSTC_RELEASE=1.98.0-nightly
EXPECTED_RUSTC_COMMIT=b30f3df3ba3c4c9de2f58f1a75dd9500b79b3f8d
EXPECTED_CARGO_FUZZ_VERSION=0.13.2

if ! [[ "$FUZZ_RUNS" =~ ^[1-9][0-9]*$ ]]; then
  echo "FUZZ_RUNS must be a positive integer" >&2
  exit 1
fi
if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "cargo-fuzz ${EXPECTED_CARGO_FUZZ_VERSION} is required" >&2
  exit 1
fi
actual_fuzz_version=$(cargo fuzz --version | awk '{print $2}')
if [[ "$actual_fuzz_version" != "$EXPECTED_CARGO_FUZZ_VERSION" ]]; then
  echo "cargo-fuzz version mismatch: expected ${EXPECTED_CARGO_FUZZ_VERSION}, got ${actual_fuzz_version}" >&2
  exit 1
fi

rustc_details=$(rustc "+${FUZZ_TOOLCHAIN}" -Vv)
rustc_release=$(awk '/^release:/ {print $2}' <<<"$rustc_details")
rustc_commit=$(awk '/^commit-hash:/ {print $2}' <<<"$rustc_details")
if [[ "$rustc_release" != "$EXPECTED_RUSTC_RELEASE" || "$rustc_commit" != "$EXPECTED_RUSTC_COMMIT" ]]; then
  echo "fuzz Rust mismatch: expected ${EXPECTED_RUSTC_RELEASE} ${EXPECTED_RUSTC_COMMIT}" >&2
  exit 1
fi

declare -A max_lengths=(
  [dice_expressions]=4096
  [durable_json]=262144
  [pack_and_event_ingestion]=262144
  [model_output]=262144
  [image_boundaries]=524288
  [public_inputs]=262144
)
targets=(
  dice_expressions
  durable_json
  pack_and_event_ingestion
  model_output
  image_boundaries
  public_inputs
)

work_dir=$(mktemp -d)
cleanup() {
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

install -d -m 0755 "$OUTPUT_DIR"
printf '%s\n' "$rustc_details" >"$OUTPUT_DIR/toolchain.txt"
cargo fuzz --version >"$OUTPUT_DIR/cargo-fuzz.txt"

for target in "${targets[@]}"; do
  corpus="$work_dir/$target"
  install -d -m 0755 "$corpus"
  cp -a -- "tests/fuzz/corpus/$target/." "$corpus/"
  ASAN_OPTIONS=symbolize=0:detect_leaks=1 \
    cargo "+${FUZZ_TOOLCHAIN}" fuzz run --fuzz-dir tests/fuzz "$target" "$corpus" -- \
    "-runs=${FUZZ_RUNS}" \
    "-max_len=${max_lengths[$target]}" \
    -timeout=10 >"$OUTPUT_DIR/$target.log" 2>&1
  if find "tests/fuzz/artifacts/$target" -type f -print -quit 2>/dev/null | grep --quiet .; then
    echo "fuzz target ${target} produced a crash artifact" >&2
    exit 1
  fi
  echo "fuzz target passed: ${target} (${FUZZ_RUNS} runs)"
done

{
  echo "toolchain=${EXPECTED_RUSTC_RELEASE}"
  echo "rustc_commit=${EXPECTED_RUSTC_COMMIT}"
  echo "cargo_fuzz=${EXPECTED_CARGO_FUZZ_VERSION}"
  echo "runs_per_target=${FUZZ_RUNS}"
  printf 'target=%s\n' "${targets[@]}"
} >"$OUTPUT_DIR/SUMMARY"

echo "fuzz smoke passed: ${#targets[@]} boundaries under AddressSanitizer"
