#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

source tests/browser/support/mongo-test-env.sh
database="${SLICE5_MONGODB_DATABASE:-mdnd_slice5_browser}"

prepare_mongo_browser_database "$database"
log_path="${PRIVATE_INSPIRATION_LOG_PATH:-target/playwright/slice5-server.log}"
mkdir -p "$(dirname "$log_path")"
: >"$log_path"
exec > >(tee "$log_path") 2>&1
exec target/release/manchester-dnd-web
