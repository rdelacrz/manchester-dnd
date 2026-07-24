#!/usr/bin/env bash

# Shared setup for browser journeys that need a disposable MongoDB database.
# The explicit root URI is accepted only on loopback because these scripts drop
# and recreate the named test database before applying the managed schema.

prepare_mongo_browser_database() {
  local database="$1"
  local uri="${MONGODB_TEST_URI:-mongodb://root:dev-root-password@127.0.0.1:27017/?authSource=admin&replicaSet=rs0&directConnection=true}"

  if [[ ! "$database" =~ ^mdnd_[a-z0-9_]+_browser$ ]]; then
    echo "browser MongoDB name must match mdnd_*_browser" >&2
    return 1
  fi
  if [[ "$uri" != mongodb://root:*@127.0.0.1:* ]]; then
    echo "MONGODB_TEST_URI must be the explicit loopback root test URI" >&2
    return 1
  fi
  if ! command -v mongosh >/dev/null 2>&1; then
    echo "browser evidence requires mongosh" >&2
    return 1
  fi

  mongosh "$uri" --quiet --eval "db.getSiblingDB('$database').dropDatabase()" >/dev/null
  MONGODB_URI="$uri" \
  MONGODB_SCHEMA_URI="$uri" \
  MONGODB_DATABASE="$database" \
    cargo run --locked --quiet -p manchester-dnd-server --bin mongo-admin -- schema apply

  export MONGODB_URI="$uri"
  export MONGODB_SCHEMA_URI="$uri"
  export MONGODB_DATABASE="$database"
}
