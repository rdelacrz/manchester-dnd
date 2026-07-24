#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

SOAK_ROUNDS=${SOAK_ROUNDS:-5}
SOAK_CASE_TIMEOUT_SECONDS=${SOAK_CASE_TIMEOUT_SECONDS:-180}
OUTPUT_DIR=${SOAK_OUTPUT_DIR:-target/release-evidence/load-soak}

if ! [[ "$SOAK_ROUNDS" =~ ^[1-9][0-9]*$ ]] || ((SOAK_ROUNDS > 100)); then
  echo "SOAK_ROUNDS must be between 1 and 100" >&2
  exit 1
fi
if ! [[ "$SOAK_CASE_TIMEOUT_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
  echo "SOAK_CASE_TIMEOUT_SECONDS must be a positive integer" >&2
  exit 1
fi
if [[ -z "${MONGODB_TEST_URI:-}" ]]; then
  echo "MONGODB_TEST_URI is required for the MongoDB load/soak gate" >&2
  exit 1
fi

labels=(
  turn_commits
  idempotent_concurrency
  history_and_export
  job_lease_recovery
  worker_concurrency
  provider_concurrency_limit
  provider_lifetime_limits
)
declare -A tests=(
  [turn_commits]='application::tests::deterministic_check_commits_and_reloads_without_rerolling'
  [idempotent_concurrency]='application::tests::concurrent_duplicate_requests_share_one_roll_and_commit'
  [history_and_export]='repository::lifecycle::tests::canonical_export_over_64k_restores_rolls_pins_provenance_and_closes_open_play'
  [job_lease_recovery]='repository::jobs::tests::expired_lease_is_closed_and_reclaimed_without_reusing_attempt'
  [worker_concurrency]='repository::jobs::tests::concurrent_workers_claim_a_job_once'
  [provider_concurrency_limit]='repository::governance::tests::concurrent_preflight_serializes_and_exact_replay_never_double_reserves'
  [provider_lifetime_limits]='repository::jobs::tests::illustration_limits_enforce_three_per_day_and_ten_per_lifetime'
)

rm -rf -- "$OUTPUT_DIR"
install -d -m 0755 "$OUTPUT_DIR/logs"
results="$OUTPUT_DIR/results.tsv"
printf 'round\tcategory\telapsed_ms\n' >"$results"

# Compile once before measuring and before parallel cargo processes contend on
# the target-directory lock.
cargo test --locked -p manchester-dnd-server --no-run \
  >"$OUTPUT_DIR/compile.log" 2>&1

suite_start=$(date +%s%3N)
for round in $(seq 1 "$SOAK_ROUNDS"); do
  pids=()
  metas=()
  for label in "${labels[@]}"; do
    log="$OUTPUT_DIR/logs/round-${round}-${label}.log"
    meta="$OUTPUT_DIR/logs/round-${round}-${label}.meta"
    test_name=${tests[$label]}
    (
      case_start=$(date +%s%3N)
      timeout "$SOAK_CASE_TIMEOUT_SECONDS" \
        cargo test --locked -p manchester-dnd-server "$test_name" -- --exact
      case_end=$(date +%s%3N)
      printf '%s\t%s\t%s\n' "$round" "$label" "$((case_end - case_start))" >"$meta"
    ) >"$log" 2>&1 &
    pids+=("$!")
    metas+=("$meta")
  done

  failed=0
  for index in "${!pids[@]}"; do
    if ! wait "${pids[$index]}"; then
      echo "load/soak case failed: round ${round}, ${labels[$index]}" >&2
      tail -n 80 "$OUTPUT_DIR/logs/round-${round}-${labels[$index]}.log" >&2 || true
      failed=1
    fi
  done
  ((failed == 0)) || exit 1
  for meta in "${metas[@]}"; do
    cat "$meta" >>"$results"
    rm -- "$meta"
  done
  echo "load/soak round passed: ${round}/${SOAK_ROUNDS} (${#labels[@]} concurrent contracts)"
done
suite_end=$(date +%s%3N)

{
  echo "schema=manchester-arcana/load-soak/v1"
  echo "rounds=${SOAK_ROUNDS}"
  echo "contracts_per_round=${#labels[@]}"
  echo "total_contract_runs=$((SOAK_ROUNDS * ${#labels[@]}))"
  echo "parallel_contracts=${#labels[@]}"
  echo "elapsed_ms=$((suite_end - suite_start))"
  printf 'category=%s\n' "${labels[@]}"
} >"$OUTPUT_DIR/SUMMARY"

echo "load/soak passed: $((SOAK_ROUNDS * ${#labels[@]})) MongoDB contract runs"
