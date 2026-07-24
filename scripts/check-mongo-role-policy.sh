#!/usr/bin/env bash
set -euo pipefail

: "${MONGODB_APP_URI:?set MONGODB_APP_URI to the application-role URL}"
: "${MONGODB_SCHEMA_URI:?set MONGODB_SCHEMA_URI to the schema-admin URL}"
: "${MONGODB_DATABASE:?set MONGODB_DATABASE to the allowlisted database name}"

case "$MONGODB_DATABASE" in
  *[!A-Za-z0-9_-]* | "" | _* | -*)
    echo "MONGODB_DATABASE fails the role-check allowlist" >&2
    exit 2
    ;;
esac

if command -v mongosh >/dev/null 2>&1; then
  mongosh_command=(mongosh)
elif command -v docker >/dev/null 2>&1 && docker compose ps --status running mongodb >/dev/null 2>&1; then
  mongosh_command=(docker compose exec -T mongodb mongosh)
else
  echo "mongosh is unavailable and the Compose MongoDB service is not running" >&2
  exit 127
fi

"${mongosh_command[@]}" "$MONGODB_APP_URI" --quiet --eval "
const target = db.getSiblingDB('$MONGODB_DATABASE');
if (target.runCommand({ping: 1}).ok !== 1) quit(1);
let denied = false;
try {
  const result = target.runCommand({collMod: '__role_policy_probe__', validator: {}});
  denied = result.ok !== 1 && result.code === 13;
} catch (error) {
  denied = error.code === 13;
}
if (!denied) {
  print('application role unexpectedly has schema mutation privileges');
  quit(1);
}
"

probe="__role_policy_probe_${$}"
"${mongosh_command[@]}" "$MONGODB_SCHEMA_URI" --quiet --eval "
const target = db.getSiblingDB('$MONGODB_DATABASE');
const name = '$probe';
target.createCollection(name);
const modified = target.runCommand({collMod: name, validator: {\$jsonSchema: {bsonType: 'object'}}});
if (modified.ok !== 1) quit(1);
if (!target.getCollection(name).drop()) quit(1);
"

echo "MongoDB role policy verified"
