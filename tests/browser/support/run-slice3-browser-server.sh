#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

source tests/browser/support/mongo-test-env.sh
database="${SLICE3_MONGODB_DATABASE:-mdnd_slice3_browser}"

prepare_mongo_browser_database "$database"
exec target/release/manchester-dnd-web
