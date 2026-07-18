#!/usr/bin/env bash
set -Eeuo pipefail

workspace="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace"

evidence_dir="${PENETRATION_EVIDENCE_DIR:-target/release-evidence/penetration-smoke}"
rm -rf "$evidence_dir"
mkdir -p "$evidence_dir"
summary="$evidence_dir/results.tsv"
printf 'case\tresult\n' >"$summary"

run_case() {
    local name="$1"
    shift
    if "$@" >"$evidence_dir/$name.log" 2>&1; then
        printf '%s\tpassed\n' "$name" >>"$summary"
    else
        printf '%s\tfailed\n' "$name" >>"$summary"
        tail -80 "$evidence_dir/$name.log" >&2
        exit 1
    fi
}

# Browser trust, CSRF-like drive-by mutation, framing, content type, request
# size, restore confirmation, and independent rate-window boundaries.
run_case http-boundaries \
    cargo test --locked -p manchester-dnd-app --features ssr --bin manchester-dnd-web
run_case server-function-origin \
    cargo test --locked -p manchester-dnd-app \
    campaign::tests::mutation_origin_must_match_the_request_host

# Hosted access remains unavailable; local identifiers cannot select another
# campaign, character, consent scope, or owner-private lifecycle object.
run_case hosted-fail-closed \
    cargo test --locked -p manchester-dnd-server hosted_mode
run_case intent-identity-scope \
    cargo test --locked -p manchester-dnd-server \
    application::tests::unknown_action_and_wrong_character_are_rejected
run_case consent-opaque-scope \
    cargo test --locked -p manchester-dnd-server \
    inspiration::tests::opaque_ids_and_versioned_commands_reject_forged_textual_scope
run_case owner-lifecycle-scope \
    cargo test --locked -p manchester-dnd-server \
    repository::lifecycle::tests::owner_scoped_lifecycle_has_exact_replay_and_play_boundaries

# Parser/upload-equivalent, path, active-content/XSS, prompt-injection, and
# bounded hostile-output checks. The MVP has no general upload endpoint.
run_case pack-path-and-file-boundaries \
    cargo test --locked -p manchester-dnd-server content::tests::traversal_and_unindexed_files_are_quarantined
run_case source-active-content-and-injection \
    cargo test --locked -p manchester-dnd-server events::tests::conservative_scanner_covers_each_compiled_finding_family
run_case typed-gm-hostile-output \
    cargo test --locked -p manchester-dnd-server typed_gm::tests::malformed_timeout_hostile_contradictory_and_over_budget_outputs_fallback
run_case strict-public-wire-types \
    cargo test --locked -p manchester-dnd-core rules_matrix::tests::strict_wire_types_reject_unknown_fields

# SSRF/redirect, malicious image bytes, traversal, selection, and authorized
# protected delivery. No provider-returned URL is fetched.
run_case provider-url-policy \
    cargo test --locked -p manchester-dnd-server scene_images::tests::provider_urls_are_never_fetched_and_are_quarantined
run_case image-parser-boundaries \
    cargo test --locked -p manchester-dnd-server scene_images::tests::image_processing_rejects_spoofed_and_oversized_inputs_and_strips_metadata
run_case artifact-path-boundary \
    cargo test --locked -p manchester-dnd-server scene_images::tests::storage_keys_reject_traversal_and_absolute_paths
run_case artifact-authorization-and-replacement \
    cargo test --locked -p manchester-dnd-server scene_images::tests::durable_image_request_replays_publishes_authorizes_and_replaces_once

run_case private-source-static-boundary scripts/check-private-inspiration-boundary.sh

case_count="$(tail -n +2 "$summary" | wc -l)"
if [[ "$case_count" -ne 15 ]] || grep -q $'\tfailed$' "$summary"; then
    echo "penetration smoke: incomplete or failed case portfolio" >&2
    exit 1
fi
printf 'penetration smoke: %s focused boundary cases passed; evidence=%s\n' \
    "$case_count" "$evidence_dir"
