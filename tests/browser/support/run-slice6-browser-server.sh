#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$repository_root"

source tests/browser/support/mongo-test-env.sh
database="${SLICE6_MONGODB_DATABASE:-mdnd_slice6_browser}"
prepare_mongo_browser_database "$database"

rm -rf .runtime-private/playwright/slice6/images
exec target/release/manchester-dnd-web
